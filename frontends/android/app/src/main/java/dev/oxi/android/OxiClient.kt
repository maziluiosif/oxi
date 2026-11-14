package dev.oxi.android

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.channels.Channel
import org.json.JSONObject
import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/** Where the Mac's oxi daemon listens (`oxi server phone` prints it). */
data class Endpoint(val host: String, val port: Int, val token: String)

sealed interface ConnState {
    data object Idle : ConnState
    data object Connecting : ConnState
    data class Connected(val version: String) : ConnState
    data class Failed(val message: String) : ConnState
}

class RpcError(message: String) : Exception(message)

private const val PING_EVERY = 15_000L
private const val PING_TIMEOUT = 6_000L

/** A server-pushed notification: `{"method": ..., "params": ...}`. */
data class Note(val method: String, val params: JSONObject)

/**
 * One JSON object per line over TCP to `oxi headless --daemon` (see src/headless/phone.rs).
 * Reconnects with backoff while [start]ed; requests made while disconnected fail fast.
 */
class OxiClient(private val scope: CoroutineScope) {
    private val _state = MutableStateFlow<ConnState>(ConnState.Idle)
    val state: StateFlow<ConnState> = _state

    private val _notes = MutableSharedFlow<Note>(extraBufferCapacity = 256)
    val notes: SharedFlow<Note> = _notes

    /** Fires each time a connection is (re)established, after the handshake. */
    private val _connected = MutableSharedFlow<Unit>(extraBufferCapacity = 4)
    val connected: SharedFlow<Unit> = _connected

    private val nextId = AtomicLong(1)
    private val pending = ConcurrentHashMap<Long, CompletableDeferred<Any?>>()
    private val writeLock = Mutex()
    @Volatile private var socket: Socket? = null
    @Volatile private var out: OutputStream? = null
    private var loop: Job? = null
    @Volatile private var endpoint: Endpoint? = null
    val current: Endpoint? get() = endpoint
    private val wake = Channel<Unit>(Channel.CONFLATED)
    @Volatile private var lastReceived = 0L

    fun start(endpoint: Endpoint) {
        if (this.endpoint == endpoint && loop?.isActive == true) return
        stop()
        this.endpoint = endpoint
        loop = scope.launch(Dispatchers.IO) { runLoop(endpoint) }
    }

    fun stop() {
        loop?.cancel()
        loop = null
        closeSocket()
        _state.value = ConnState.Idle
    }

    /**
     * The phone switched networks (Wi-Fi ↔ mobile data): the old socket is bound to the network
     * that went away and would only notice after a long TCP timeout, so start over right away.
     */
    fun reconnect() {
        if (loop?.isActive != true) return
        closeSocket()
        // Until the loop notices: nothing can be asked over the socket that was just closed.
        if (_state.value is ConnState.Connected) _state.value = ConnState.Connecting
        wake.trySend(Unit)
    }

    /** Suspends until the handshake is done; false when that takes longer than [timeoutMs]. */
    suspend fun awaitConnected(timeoutMs: Long): Boolean =
        withTimeoutOrNull(timeoutMs) { state.first { it is ConnState.Connected } } != null

    /**
     * The app came back. Android freezes cached apps, so a socket idle since before then is
     * likely dead: start over. A recent one gets a ping instead of waiting for the heartbeat.
     */
    fun kick() {
        val quiet = System.currentTimeMillis() - lastReceived > PING_EVERY + PING_TIMEOUT
        if (_state.value is ConnState.Connected && !quiet) scope.launch(Dispatchers.IO) { ping() } else reconnect()
    }

    /** A request that must answer quickly; dropping the socket on silence triggers a reconnect. */
    private suspend fun ping() {
        try {
            withTimeout(PING_TIMEOUT) { request("ping") }
        } catch (e: RpcError) {
            // Answered, just not understood (an older daemon): still alive.
            if (e.message == "Not connected" || e.message?.startsWith("Send failed") == true) closeSocket()
        } catch (e: Exception) {
            closeSocket()
        }
    }

    private fun closeSocket() {
        runCatching { socket?.close() }
        socket = null
        out = null
        val error = RpcError("Disconnected")
        pending.values.forEach { it.completeExceptionally(error) }
        pending.clear()
    }

    private suspend fun runLoop(endpoint: Endpoint) {
        var backoff = 500L
        while (scope.isActive) {
            _state.value = ConnState.Connecting
            try {
                var greeted = false
                val s = Socket()
                s.tcpNoDelay = true
                s.keepAlive = true
                s.connect(InetSocketAddress(endpoint.host, endpoint.port), 8000)
                socket = s
                out = s.getOutputStream()
                val reader = BufferedReader(InputStreamReader(s.getInputStream(), Charsets.UTF_8))
                val heartbeat = scope.launch(Dispatchers.IO) {
                    while (true) {
                        delay(PING_EVERY)
                        if (_state.value is ConnState.Connected) ping()
                    }
                }
                val hello = scope.launch(Dispatchers.IO) {
                    try {
                        val result = withTimeout(10_000) {
                            request("daemon.hello", JSONObject().put("token", endpoint.token).put("client", "android"))
                        } as JSONObject
                        greeted = true
                        _state.value = ConnState.Connected(result.optString("version"))
                        _connected.tryEmit(Unit)
                    } catch (e: Exception) {
                        _state.value = ConnState.Failed("Handshake failed: ${e.message}")
                        runCatching { s.close() }
                    }
                }
                backoff = 500L
                while (true) {
                    val line = reader.readLine() ?: break
                    if (line.isBlank()) continue
                    dispatch(line)
                }
                hello.cancel()
                heartbeat.cancel()
                _state.value = ConnState.Failed(
                    if (greeted) "Connection lost" else "Connection closed (wrong token?)"
                )
            } catch (e: Exception) {
                _state.value = ConnState.Failed(e.message ?: e.javaClass.simpleName)
            } finally {
                closeSocket()
            }
            // Wait before trying again, unless a network change says to try now.
            while (wake.tryReceive().isSuccess) Unit
            if (withTimeoutOrNull(backoff) { wake.receive() } != null) backoff = 500L
            else backoff = (backoff * 2).coerceAtMost(8000L)
        }
    }

    private fun dispatch(line: String) {
        lastReceived = System.currentTimeMillis()
        val obj = runCatching { JSONObject(line) }.getOrNull() ?: return
        if (obj.has("id") && !obj.isNull("id") && !obj.has("method")) {
            val id = obj.optLong("id", -1)
            val waiter = pending.remove(id) ?: return
            if (obj.has("error") && !obj.isNull("error")) {
                waiter.completeExceptionally(RpcError(obj.opt("error")?.toString() ?: "error"))
            } else {
                waiter.complete(if (obj.isNull("result")) null else obj.get("result"))
            }
            return
        }
        val method = obj.optString("method")
        if (method.isNotEmpty()) {
            _notes.tryEmit(Note(method, obj.optJSONObject("params") ?: JSONObject()))
        }
    }

    /** Send a request; the result is a JSONObject, JSONArray, primitive or null. */
    suspend fun request(method: String, params: JSONObject = JSONObject()): Any? {
        val stream = out ?: throw RpcError("Not connected")
        val id = nextId.getAndIncrement()
        val waiter = CompletableDeferred<Any?>()
        pending[id] = waiter
        val line = JSONObject().put("id", id).put("method", method).put("params", params).toString() + "\n"
        try {
            withContext(Dispatchers.IO) {
                writeLock.withLock {
                    stream.write(line.toByteArray(Charsets.UTF_8))
                    stream.flush()
                }
            }
        } catch (e: Exception) {
            pending.remove(id)
            throw RpcError("Send failed: ${e.message}")
        }
        return waiter.await()
    }
}

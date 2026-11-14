package dev.oxi.android

import android.app.Application
import android.content.Context
import android.content.SharedPreferences
import android.net.ConnectivityManager
import android.net.Network
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import org.json.JSONArray
import org.json.JSONObject

/** A paired machine running `oxi server`. */
data class Server(val id: String, val name: String, val host: String, val port: Int, val token: String) {
    val endpoint get() = Endpoint(host, port, token)

    fun toJson(): JSONObject = JSONObject().put("id", id).put("name", name).put("host", host)
        .put("port", port).put("token", token)

    companion object {
        fun fromJson(o: JSONObject) = Server(
            o.optString("id"), o.optString("name"), o.optString("host"), o.optInt("port", 47811), o.optString("token"),
        )
    }
}

/** The paired servers, in the app's preferences. */
object ServerStore {
    fun prefs(context: Context): SharedPreferences = context.getSharedPreferences("oxi", Context.MODE_PRIVATE)

    fun newId() = java.util.UUID.randomUUID().toString().take(8)

    fun load(context: Context): List<Server> {
        val prefs = prefs(context)
        prefs.getString("servers", null)?.let { text ->
            val arr = runCatching { JSONArray(text) }.getOrNull() ?: return emptyList()
            return (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(Server::fromJson) }
        }
        // The first version kept a single server.
        val host = prefs.getString("host", null) ?: return emptyList()
        val token = prefs.getString("token", null) ?: return emptyList()
        val server = Server(newId(), host, host, prefs.getInt("port", 47811), token)
        prefs.edit().remove("host").remove("token").remove("port")
            .putString("root:${server.id}", prefs.getString("root", null)).remove("root").apply()
        save(context, listOf(server))
        return listOf(server)
    }

    fun save(context: Context, servers: List<Server>) {
        prefs(context).edit().putString("servers", JSONArray(servers.map { it.toJson() }).toString()).apply()
    }

    /** The server the app last talked to. */
    fun active(context: Context): Server? {
        val servers = load(context)
        val id = prefs(context).getString("active", null)
        return servers.firstOrNull { it.id == id } ?: servers.firstOrNull()
    }
}

/**
 * The connection belongs to the process, not to a screen: it outlives the activity while
 * [TurnService] keeps the process up for a running turn, and notification actions use it
 * without any screen at all.
 */
class OxiApp : Application() {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    lateinit var client: OxiClient
        private set
    lateinit var watch: TurnWatch
        private set

    override fun onCreate() {
        super.onCreate()
        client = OxiClient(scope)
        watch = TurnWatch(this)
        watchNetwork()
    }

    /** Make sure a connection to the saved server is up or being made. */
    fun connect() {
        if (client.current == null) ServerStore.active(this)?.let { client.start(it.endpoint) } else client.kick()
    }

    /** Reconnect as soon as the default network changes (Wi-Fi ↔ mobile data, Tailscale up). */
    private fun watchNetwork() {
        val cm = getSystemService(ConnectivityManager::class.java) ?: return
        var current: Network? = null
        runCatching {
            cm.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    val previous = current
                    current = network
                    if (previous != null && previous != network) client.reconnect()
                }

                override fun onLost(network: Network) {
                    if (network == current) client.reconnect()
                }
            })
        }
    }
}

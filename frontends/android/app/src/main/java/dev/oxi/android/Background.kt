package dev.oxi.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.RemoteInput
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import org.json.JSONArray
import org.json.JSONObject

/** A chat this phone is waiting on. */
data class Followed(val root: String, val title: String, val running: Boolean)

/** A short plain line out of a markdown answer, for a notification. */
fun snippet(markdown: String, max: Int = 360): String {
    val text = markdown.lineSequence()
        .filterNot { it.trimStart().startsWith("```") }
        .joinToString("\n") { it.trimStart().trimStart('#', '>', ' ') }
        .replace("**", "").replace("`", "")
        .trim()
    return if (text.length <= max) text else text.take(max).trimEnd() + "…"
}

/**
 * Follows the chats the phone sent prompts to once the app is no longer on screen: keeps the
 * connection alive through [TurnService] while they run, and notifies when one finishes or
 * waits for an approval.
 */
class TurnWatch(private val app: OxiApp) {
    private val manager = app.getSystemService(NotificationManager::class.java)
    private val followed = HashMap<String, Followed>()
    /** The reply so far of each followed chat, to quote when it finishes. */
    private val answers = HashMap<String, String>()
    /** Approvals still unanswered, to notify about once the app leaves the screen. */
    private val approvals = HashMap<String, JSONObject>()
    private var stopJob: Job? = null

    init {
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStop(owner: LifecycleOwner) {
                approvals.forEach { (id, approval) -> followed[id]?.let { notifyApproval(id, it, approval) } }
            }
        })
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_WORKING, "Agent working", NotificationManager.IMPORTANCE_LOW).apply {
                description = "Shown while oxi waits for a running agent"
                setShowBadge(false)
            }
        )
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_DONE, "Finished replies", NotificationManager.IMPORTANCE_DEFAULT)
        )
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_APPROVAL, "Approvals and questions", NotificationManager.IMPORTANCE_HIGH)
        )
        app.scope.launch { app.client.notes.collect(::onNote) }
        app.scope.launch { app.client.connected.collect { resync() } }
    }

    private fun onScreen() = ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)

    /** The app shows chat [id] of [root]; [running] while a turn is under way. */
    fun follow(root: String, id: String, title: String, running: Boolean) {
        val known = followed[id]
        if (!running && known == null) return
        val next = Followed(root, title.ifBlank { known?.title.orEmpty() }, running || known?.running == true)
        if (next == known) return
        followed[id] = next
        if (running) clear(id)
        updateService()
    }

    /** Another server was picked: nothing here is followed any more. */
    fun reset() {
        followed.keys.toList().forEach(::clear)
        followed.clear()
        answers.clear()
        approvals.clear()
        updateService()
    }

    /** Notifications were just allowed: show the one a running service couldn't post before. */
    fun permissionGranted() {
        if (TurnService.running) runCatching { manager.notify(ID_WORKING, working()) }
    }

    /** Drop the notifications of chat [id]: it is on screen, or was answered. */
    fun clear(id: String) {
        manager.cancel(TAG_DONE, id.hashCode())
        manager.cancel(TAG_APPROVAL, id.hashCode())
    }

    private fun onNote(note: Note) {
        val p = note.params
        val id = p.optString("chat")
        val chat = followed[id] ?: return
        when (note.method) {
            "turn" -> {
                val running = p.optBoolean("running")
                if (running) {
                    if (!chat.running) followed[id] = chat.copy(running = true)
                    updateService()
                } else if (p.optInt("queued") == 0) {
                    val error = p.optString("error").takeIf { !p.isNull("error") && it.isNotEmpty() }
                    finished(id, chat, p.optString("outcome"), error)
                }
            }
            "approval" -> {
                val approval = p.optJSONObject("approval")
                if (approval == null) {
                    approvals.remove(id)
                    manager.cancel(TAG_APPROVAL, id.hashCode())
                } else {
                    approvals[id] = approval
                    if (!onScreen()) notifyApproval(id, chat, approval)
                }
            }
            "title" -> followed[id] = chat.copy(title = p.optString("title").ifBlank { chat.title })
            "message" -> {
                val message = p.optJSONObject("message") ?: return
                if (message.optString("role") == "user") return
                lastAnswer(message.optJSONArray("blocks"))?.let { answers[id] = it }
            }
            "chat_removed" -> {
                followed.remove(id)
                answers.remove(id)
                approvals.remove(id)
                clear(id)
                updateService()
            }
        }
    }

    private fun lastAnswer(blocks: JSONArray?): String? {
        if (blocks == null) return null
        for (i in blocks.length() - 1 downTo 0) {
            val b = blocks.optJSONObject(i) ?: continue
            if (b.optString("type") == "answer" && b.optString("text").isNotBlank()) return b.optString("text")
        }
        return null
    }

    private fun finished(id: String, chat: Followed, outcome: String, error: String?) {
        followed.remove(id)
        approvals.remove(id)
        val answer = answers.remove(id)
        manager.cancel(TAG_APPROVAL, id.hashCode())
        if (!onScreen() && outcome != "cancelled") notifyDone(id, chat, answer, error)
        updateService()
    }

    /** Back online: a turn may have ended while the connection was down. */
    private suspend fun resync() {
        val running = followed.filterValues { it.running }
        for (root in running.values.map { it.root }.distinct()) {
            val chats = runCatching { app.client.request("chats.list", JSONObject().put("root", root)) as JSONArray }
                .getOrNull() ?: continue
            val live = parseChats(chats).filter { it.running }.map { it.id }.toSet()
            running.filter { (id, chat) -> chat.root == root && id !in live }.forEach { (id, _) ->
                // Unless a note said so meanwhile.
                followed[id]?.takeIf { it.running }?.let { finished(id, it, "", null) }
            }
        }
    }

    // ---- the service that keeps the process and its connection up ---------------------------

    private fun updateService() {
        val running = followed.values.count { it.running }
        if (running == 0) {
            if (!TurnService.running || stopJob?.isActive == true) return
            // A queued prompt starts right after the turn before it: don't stop in between.
            stopJob = app.scope.launch {
                delay(4000)
                if (followed.values.none { it.running }) app.stopService(Intent(app, TurnService::class.java))
            }
            return
        }
        stopJob?.cancel()
        if (TurnService.running) {
            manager.notify(ID_WORKING, working())
        } else {
            // Refused when the app is in the background and no notification brought us here.
            runCatching { ContextCompat.startForegroundService(app, Intent(app, TurnService::class.java)) }
        }
    }

    fun working(): Notification {
        val running = followed.filterValues { it.running }
        val (id, first) = running.entries.firstOrNull()?.let { it.key to it.value } ?: (null to null)
        val text = when {
            first == null -> "Waiting for the agent"
            running.size == 1 -> first.title.ifBlank { "Waiting for the agent" }
            else -> "${running.size} chats"
        }
        return NotificationCompat.Builder(app, CHANNEL_WORKING)
            .setSmallIcon(R.drawable.ic_stat_oxi)
            .setContentTitle("oxi is working")
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setProgress(0, 0, true)
            .setColor(ACCENT)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .setContentIntent(if (id != null && first != null && running.size == 1) open(id, first) else openApp())
            .build()
    }

    // ---- notifications ----------------------------------------------------------------------

    private fun openApp(): PendingIntent = PendingIntent.getActivity(
        app, 0,
        Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    private fun open(id: String, chat: Followed): PendingIntent = PendingIntent.getActivity(
        app, id.hashCode(),
        Intent(app, MainActivity::class.java).setAction(ACTION_OPEN_CHAT)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
            .putExtra(EXTRA_CHAT, id).putExtra(EXTRA_ROOT, chat.root).putExtra(EXTRA_TITLE, chat.title),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    private fun action(name: String, id: String, chat: Followed, decision: String?, mutable: Boolean): PendingIntent {
        val intent = Intent(app, NoteActionReceiver::class.java).setAction(name)
            .putExtra(EXTRA_CHAT, id).putExtra(EXTRA_ROOT, chat.root).putExtra(EXTRA_TITLE, chat.title)
        if (decision != null) intent.putExtra(EXTRA_DECISION, decision)
        // One per chat and choice: extras alone don't tell PendingIntents apart.
        val request = 31 * id.hashCode() + (decision ?: name).hashCode()
        val flags = PendingIntent.FLAG_UPDATE_CURRENT or
            if (mutable) PendingIntent.FLAG_MUTABLE else PendingIntent.FLAG_IMMUTABLE
        return PendingIntent.getBroadcast(app, request, intent, flags)
    }

    private fun notifyDone(id: String, chat: Followed, answer: String?, error: String?) {
        val text = error ?: answer?.let { snippet(it) } ?: "The reply is ready"
        val reply = NotificationCompat.Action.Builder(
            R.drawable.ic_stat_oxi, "Reply", action(ACTION_REPLY, id, chat, null, mutable = true),
        ).addRemoteInput(RemoteInput.Builder(KEY_REPLY).setLabel("Follow up…").build())
            .setAllowGeneratedReplies(false)
            .build()
        val builder = NotificationCompat.Builder(app, CHANNEL_DONE)
            .setSmallIcon(R.drawable.ic_stat_oxi)
            .setContentTitle(chat.title.ifBlank { "oxi" })
            .setSubText(if (error != null) "Stopped" else chat.root.substringAfterLast('/'))
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setColor(if (error != null) ERROR else ACCENT)
            .setAutoCancel(true)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setContentIntent(open(id, chat))
            .addAction(reply)
        post(TAG_DONE, id, builder.build())
    }

    private fun notifyApproval(id: String, chat: Followed, approval: JSONObject) {
        val question = approval.optJSONObject("elicitation") != null
        val summary = approval.optString("summary")
        val builder = NotificationCompat.Builder(app, CHANNEL_APPROVAL)
            .setSmallIcon(R.drawable.ic_stat_oxi)
            .setContentTitle(if (question) "oxi has a question" else "Allow ${approval.optString("tool")}?")
            .setSubText(chat.title.ifBlank { chat.root.substringAfterLast('/') })
            .setContentText(summary)
            .setStyle(NotificationCompat.BigTextStyle().bigText(summary))
            .setColor(ACCENT)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_REMINDER)
            .setContentIntent(open(id, chat))
        if (!question) {
            // Letting a tool run takes an unlocked phone; turning it down doesn't.
            builder.addAction(
                NotificationCompat.Action.Builder(
                    R.drawable.ic_stat_oxi, "Allow", action(ACTION_RESPOND, id, chat, "approve", mutable = false),
                ).setAuthenticationRequired(true).build()
            )
            builder.addAction(R.drawable.ic_stat_oxi, "Deny", action(ACTION_RESPOND, id, chat, "deny", mutable = false))
        }
        post(TAG_APPROVAL, id, builder.build())
    }

    /** Tell that a notification's action didn't go through; tapping opens the chat instead. */
    fun actionFailed(id: String, root: String, title: String, message: String) {
        val chat = Followed(root, title, false)
        val builder = NotificationCompat.Builder(app, CHANNEL_DONE)
            .setSmallIcon(R.drawable.ic_stat_oxi)
            .setContentTitle(title.ifBlank { "oxi" })
            .setContentText(message)
            .setColor(ERROR)
            .setAutoCancel(true)
            .setContentIntent(open(id, chat))
        clear(id)
        post(TAG_DONE, id, builder.build())
    }

    private fun post(tag: String, id: String, notification: Notification) {
        // Without the notification permission this throws nothing and shows nothing.
        runCatching { manager.notify(tag, id.hashCode(), notification) }
    }

    companion object {
        const val CHANNEL_WORKING = "working"
        const val CHANNEL_DONE = "done"
        const val CHANNEL_APPROVAL = "approval"
        const val ID_WORKING = 1
        const val TAG_DONE = "done"
        const val TAG_APPROVAL = "approval"
        const val ACTION_OPEN_CHAT = "dev.oxi.android.OPEN_CHAT"
        const val ACTION_RESPOND = "dev.oxi.android.RESPOND"
        const val ACTION_REPLY = "dev.oxi.android.REPLY"
        const val EXTRA_CHAT = "chat"
        const val EXTRA_ROOT = "root"
        const val EXTRA_TITLE = "title"
        const val EXTRA_DECISION = "decision"
        const val KEY_REPLY = "text"
        private const val ACCENT = 0xFFE8833A.toInt()
        private const val ERROR = 0xFFE5645A.toInt()
    }
}

/** Keeps the process, and with it the connection, alive while a followed chat runs. */
class TurnService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        running = true
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        runCatching {
            ServiceCompat.startForeground(this, TurnWatch.ID_WORKING, (application as OxiApp).watch.working(), type)
        }.onFailure { stopSelf() }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        super.onDestroy()
    }

    companion object {
        @Volatile var running = false
            private set
    }
}

/** Allow, Deny and Reply, straight from a notification. */
class NoteActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val app = context.applicationContext as OxiApp
        val id = intent.getStringExtra(TurnWatch.EXTRA_CHAT) ?: return
        val root = intent.getStringExtra(TurnWatch.EXTRA_ROOT).orEmpty()
        val title = intent.getStringExtra(TurnWatch.EXTRA_TITLE).orEmpty()
        val reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(TurnWatch.KEY_REPLY)?.toString()?.trim()
        val (method, params) = when (intent.action) {
            TurnWatch.ACTION_RESPOND -> "approval.respond" to
                JSONObject().put("chat", id).put("decision", intent.getStringExtra(TurnWatch.EXTRA_DECISION))
            TurnWatch.ACTION_REPLY -> {
                if (reply.isNullOrEmpty()) return app.watch.clear(id)
                "prompt.send" to JSONObject().put("root", root).put("chat", id).put("text", reply)
            }
            else -> return
        }
        val pending = goAsync()
        app.scope.launch {
            try {
                app.connect()
                // A broadcast gets about ten seconds.
                withTimeout(8500) {
                    var sent = false
                    while (!sent) {
                        app.client.awaitConnected(8000)
                        try {
                            app.client.request(method, params)
                            sent = true
                        } catch (e: RpcError) {
                            // The socket was a dead one from before the process slept: once more.
                            if (e.message != "Not connected" && e.message != "Disconnected" && e.message?.startsWith("Send failed") != true) throw e
                            delay(300)
                        }
                    }
                }
                app.watch.clear(id)
                if (method == "prompt.send") app.watch.follow(root, id, title, running = true)
            } catch (e: Exception) {
                val why = if (e is RpcError) e.message.orEmpty() else "Can't reach the server"
                app.watch.actionFailed(id, root, title, if (reply != null) "Not sent: $why" else "Not answered: $why")
            } finally {
                pending.finish()
            }
        }
    }
}

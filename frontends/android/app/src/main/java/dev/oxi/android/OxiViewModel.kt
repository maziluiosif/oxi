package dev.oxi.android

import android.app.Application
import android.content.Context
import android.net.Uri
import androidx.core.content.FileProvider
import androidx.lifecycle.SavedStateHandle
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/** The chat on screen. `id == null` until the first prompt of a new chat comes back. */
data class OpenChat(
    val root: String,
    val id: String?,
    val title: String,
    val config: ChatConfig?,
    val messages: List<Message> = emptyList(),
    val running: Boolean = false,
    val queued: Int = 0,
    val approval: Approval? = null,
    val status: String? = null,
    val error: String? = null,
    val loading: Boolean = false,
    val sending: Boolean = false,
    /** Picked in the composer for the next prompt, and how many are still being read. */
    val attachments: List<PendingAttachment> = emptyList(),
    val attaching: Int = 0,
    /** Evidence mode for the next prompt. */
    val evidence: Boolean = false,
    /** Plan mode for the next prompt: the agent proposes before it changes anything. */
    val plan: Boolean = false,
    /** Tokens of the agent's context window in use, and its size. */
    val context: Pair<Long, Long>? = null,
    /** Bumped when the draft was changed from outside the composer (a share, a failed send). */
    val draftSeed: Int = 0,
) {
    val contextPct: Int? get() = context?.takeIf { it.second > 0 }?.let { (it.first * 100 / it.second).toInt().coerceIn(0, 100) }
}

/** Shared into oxi from another app, waiting for a chat to go to. */
data class Share(val text: String = "", val attachments: List<PendingAttachment> = emptyList(), val loading: Int = 0) {
    val count get() = attachments.size + loading + if (text.isNotBlank()) 1 else 0
}

/** The source control sheet of a workspace. */
data class GitView(
    val root: String,
    val state: GitState? = null,
    /** What is being done, to show while waiting. */
    val busy: String? = null,
    val error: String? = null,
    /** A commit message written by the server's model, and a counter to notice a new one. */
    val generated: String = "",
    val generatedSeed: Int = 0,
    val generating: Boolean = false,
)

sealed interface UpdateState {
    data object None : UpdateState
    data class Downloading(val version: String, val progress: Float) : UpdateState
    data class Ready(val version: String, val apk: java.io.File) : UpdateState
    data class Installing(val version: String) : UpdateState
    data class Failed(val version: String, val message: String) : UpdateState
}

data class UiState(
    val update: UpdateState = UpdateState.None,
    val servers: List<Server> = emptyList(),
    val activeId: String? = null,
    /** The add-server screen is open (it is the only screen while no server is paired). */
    val adding: Boolean = false,
    val conn: ConnState = ConnState.Idle,
    val host: String = "",
    val home: String? = null,
    val providers: List<Provider> = emptyList(),
    val workspaces: List<Workspace> = emptyList(),
    val workspacesLoaded: Boolean = false,
    val root: String? = null,
    val chats: List<ChatSummary> = emptyList(),
    val chatsLoading: Boolean = false,
    val chat: OpenChat? = null,
    val models: Map<String, List<String>> = emptyMap(),
    val quota: Map<String, List<QuotaWindow>> = emptyMap(),
    /** What the server supports beyond the basics (`initialize`'s `features`). */
    val features: Set<String> = emptySet(),
    val toast: String? = null,
    val share: Share? = null,
    val git: GitView? = null,
) {
    val active: Server? get() = servers.firstOrNull { it.id == activeId }
    val endpoint: Endpoint? get() = active?.endpoint
}

class OxiViewModel(app: Application, private val saved: SavedStateHandle) : AndroidViewModel(app) {
    private val prefs = ServerStore.prefs(app)
    private val oxi = app as OxiApp
    private val client = oxi.client
    private val watch = oxi.watch
    private val _ui = MutableStateFlow(UiState())
    val ui: StateFlow<UiState> = _ui

    /** Images shown in chats, by [chatImageKey] or [fileImageKey]; null when it can't be shown. */
    private val _images = MutableStateFlow<Map<String, ImageBitmap?>>(emptyMap())
    val images: StateFlow<Map<String, ImageBitmap?>> = _images

    /** What is typed and not sent, by server, folder and chat. Not in [ui]: it changes per key. */
    private val drafts = HashMap<String, String>()

    init {
        viewModelScope.launch { client.state.collect { s -> _ui.update { it.copy(conn = s) } } }
        viewModelScope.launch { client.connected.collect { onConnected() } }
        viewModelScope.launch { client.notes.collect(::onNote) }
        val servers = ServerStore.load(app)
        val active = prefs.getString("active", null)?.takeIf { id -> servers.any { it.id == id } } ?: servers.firstOrNull()?.id
        _ui.update { it.copy(servers = servers) }
        loadDrafts()
        active?.let { switchServer(it, restore = true) }
    }

    // ---- servers ---------------------------------------------------------------------------

    private fun saveServers(servers: List<Server>) = ServerStore.save(getApplication(), servers)

    private fun newId() = ServerStore.newId()

    /** Add a server, or update the one at the same address, and switch to it. */
    fun addServer(endpoint: Endpoint, name: String?) {
        val servers = _ui.value.servers
        val existing = servers.firstOrNull { it.host == endpoint.host && it.port == endpoint.port }
        val server = Server(
            existing?.id ?: newId(),
            name?.takeIf { it.isNotBlank() } ?: existing?.name ?: endpoint.host,
            endpoint.host, endpoint.port, endpoint.token,
        )
        val updated = if (existing != null) servers.map { if (it.id == server.id) server else it } else servers + server
        saveServers(updated)
        _ui.update { it.copy(servers = updated, adding = false) }
        switchServer(server.id, force = true)
    }

    /** `oxi://pair?host=..&port=..&token=..&name=..`, from a QR code, a link or a paste. */
    fun pairFromText(text: String): Boolean {
        val uri = runCatching { Uri.parse(text.trim()) }.getOrNull() ?: return false
        return pairFromUri(uri)
    }

    fun pairFromUri(uri: Uri): Boolean {
        if (uri.scheme != "oxi" || uri.host != "pair") return false
        val host = uri.getQueryParameter("host") ?: return false
        val token = uri.getQueryParameter("token") ?: return false
        val port = uri.getQueryParameter("port")?.toIntOrNull() ?: 47811
        addServer(Endpoint(host, port, token), uri.getQueryParameter("name"))
        return true
    }

    /** [restore]: the screen comes back (a new activity, or the process after Android killed it). */
    fun switchServer(id: String, force: Boolean = false, restore: Boolean = false) {
        val server = _ui.value.servers.firstOrNull { it.id == id } ?: return
        if (!force && _ui.value.activeId == id) return
        prefs.edit().putString("active", id).apply()
        val root = prefs.getString("root:$id", null)
        // The chat that was open, e.g. when the camera app pushed oxi out of memory.
        val chat = saved.get<String>("chat")?.takeIf { restore && root != null && saved.get<String>("chat_root") == root }
        if (!restore) {
            saved.remove<String>("chat")
            if (client.current != server.endpoint) watch.reset()
        }
        _ui.update {
            UiState(
                update = it.update,
                servers = it.servers,
                activeId = id,
                root = root,
                chat = chat?.let { c ->
                    if (c.isEmpty()) OpenChat(root!!, null, "New chat", null) else OpenChat(root!!, c, "", null, loading = true)
                },
                toast = it.toast,
                share = it.share,
            )
        }
        // The connection belongs to the app: it may be up already, for this very server.
        client.start(server.endpoint)
        if (client.state.value is ConnState.Connected) viewModelScope.launch { onConnected() }
    }

    fun renameServer(id: String, name: String) {
        val updated = _ui.value.servers.map { if (it.id == id) it.copy(name = name.trim().ifBlank { it.name }) else it }
        saveServers(updated)
        _ui.update { it.copy(servers = updated) }
    }

    fun removeServer(id: String) {
        val updated = _ui.value.servers.filterNot { it.id == id }
        saveServers(updated)
        prefs.edit().remove("root:$id").apply()
        if (_ui.value.activeId == id) {
            client.stop()
            watch.reset()
            _ui.update { UiState(update = it.update, servers = updated) }
            updated.firstOrNull()?.let { switchServer(it.id) } ?: prefs.edit().remove("active").apply()
        } else {
            _ui.update { it.copy(servers = updated) }
        }
    }

    fun showAddServer(show: Boolean) = _ui.update { it.copy(adding = show) }

    fun onResume() {
        client.kick()
        checkForUpdate()
    }

    // ---- self-update -----------------------------------------------------------------------

    private var lastUpdateCheck = 0L

    /** Look for a newer release at most every few hours and download it in the background. */
    fun checkForUpdate(force: Boolean = false) {
        val now = System.currentTimeMillis()
        if (!force && now - lastUpdateCheck < 4 * 3600_000L) return
        if (_ui.value.update is UpdateState.Downloading || _ui.value.update is UpdateState.Installing) return
        lastUpdateCheck = now
        viewModelScope.launch {
            val release = runCatching { Updater.check() }.getOrNull() ?: return@launch
            val context = getApplication<Application>()
            _ui.update { it.copy(update = UpdateState.Downloading(release.version, 0f)) }
            runCatching {
                Updater.download(context, release.version) { p ->
                    _ui.update { it.copy(update = UpdateState.Downloading(release.version, p)) }
                }
            }.onSuccess { apk -> _ui.update { it.copy(update = UpdateState.Ready(release.version, apk)) } }
                .onFailure { e -> _ui.update { it.copy(update = UpdateState.Failed(release.version, e.message ?: "download failed")) } }
        }
    }

    /** Install the downloaded update. Returns false when Android first needs the user's OK. */
    fun installUpdate(): Boolean {
        val ready = _ui.value.update as? UpdateState.Ready ?: return true
        val context = getApplication<Application>()
        if (!Updater.canInstall(context)) return false
        _ui.update { it.copy(update = UpdateState.Installing(ready.version)) }
        runCatching { Updater.install(context, ready.apk) }
            .onFailure { e -> _ui.update { it.copy(update = UpdateState.Failed(ready.version, e.message ?: "install failed")) } }
        return true
    }

    fun updateFailed(message: String) {
        val version = when (val u = _ui.value.update) {
            is UpdateState.Installing -> u.version
            is UpdateState.Ready -> u.version
            else -> return toast(message)
        }
        _ui.update { it.copy(update = UpdateState.Failed(version, message)) }
    }

    /** Back to "ready" after the user dismissed Android's confirmation. */
    fun updateDismissed() {
        val u = _ui.value.update as? UpdateState.Installing ?: return
        val context = getApplication<Application>()
        val apk = java.io.File(context.cacheDir, "update/oxi-${u.version}.apk")
        if (apk.exists()) _ui.update { it.copy(update = UpdateState.Ready(u.version, apk)) }
    }

    private suspend fun onConnected() {
        runCatching {
            val init = client.request("initialize") as JSONObject
            val info = client.request("server.info") as? JSONObject
            _ui.update {
                it.copy(
                    providers = parseProviders(init),
                    features = parseFeatures(init),
                    host = info?.optString("host").orEmpty(),
                    home = info?.optString("home"),
                )
            }
            val hostName = info?.optString("host").orEmpty()
            _ui.value.active?.let { server ->
                if (hostName.isNotBlank() && server.name == server.host) renameServer(server.id, hostName)
            }
        }.onFailure { toast(it.message) }
        refreshWorkspaces()
        _ui.value.root?.let { refreshChats(it) }
        _ui.value.chat?.let { chat ->
            if (chat.id != null) reopen(chat.root, chat.id) else if (chat.config == null) loadDefaultConfig(chat.root)
        }
    }

    // ---- workspaces and chats --------------------------------------------------------------

    fun refreshWorkspaces() = viewModelScope.launch {
        runCatching { client.request("workspaces.list") as JSONArray }
            .onSuccess { arr -> _ui.update { it.copy(workspaces = parseWorkspaces(arr), workspacesLoaded = true) } }
            .onFailure { toast(it.message) }
    }

    fun openWorkspace(path: String?) {
        _ui.value.activeId?.let { prefs.edit().putString("root:$it", path).apply() }
        persistDrafts()
        _ui.update { it.copy(root = path, chats = emptyList(), chat = null, git = null) }
        rememberChat()
        if (path != null) refreshChats(path)
    }

    fun refreshChats(root: String) = viewModelScope.launch {
        _ui.update { it.copy(chatsLoading = true) }
        runCatching { client.request("chats.list", JSONObject().put("root", root)) as JSONArray }
            .onSuccess { arr ->
                _ui.update { if (it.root == root) it.copy(chats = parseChats(arr), chatsLoading = false) else it }
            }
            .onFailure { e -> _ui.update { it.copy(chatsLoading = false) }; toast(e.message) }
    }

    fun listFolder(path: String, done: (String, String?, List<String>) -> Unit) = viewModelScope.launch {
        runCatching { client.request("fs.list", JSONObject().put("path", path)) as JSONObject }
            .onSuccess { obj ->
                val entries = obj.optJSONArray("entries")
                val dirs = (0 until (entries?.length() ?: 0)).mapNotNull { i ->
                    entries!!.optJSONObject(i)?.takeIf { it.optBoolean("dir") }?.optString("name")
                }
                done(obj.optString("path"), obj.optString("parent").takeIf { it.isNotEmpty() && !obj.isNull("parent") }, dirs)
            }
            .onFailure { toast(it.message) }
    }

    fun newChat() {
        val root = _ui.value.root ?: return
        val last = _ui.value.chat?.config
        _ui.update { it.copy(chat = OpenChat(root, null, "New chat", last)) }
        rememberChat()
        deliverShare()
        loadDefaultConfig(root)
    }

    fun openChat(id: String) {
        val root = _ui.value.root ?: return
        val title = _ui.value.chats.firstOrNull { it.id == id }?.title ?: ""
        _ui.update { it.copy(chat = OpenChat(root, id, title, null, loading = true)) }
        rememberChat()
        watch.clear(id)
        deliverShare()
        viewModelScope.launch { reopen(root, id) }
    }

    /** From a notification: the chat may be in another folder than the one on screen. */
    fun openChatAt(root: String, id: String, title: String) {
        if (root.isBlank() || _ui.value.activeId == null) return
        if (_ui.value.chat?.id == id) return watch.clear(id)
        persistDrafts()
        _ui.value.activeId?.let { prefs.edit().putString("root:$it", root).apply() }
        _ui.update { s ->
            s.copy(root = root, chats = if (s.root == root) s.chats else emptyList(), chat = OpenChat(root, id, title, null, loading = true), git = null)
        }
        rememberChat()
        watch.clear(id)
        deliverShare()
        refreshChats(root)
        viewModelScope.launch { reopen(root, id) }
    }

    /** Keep the open chat (empty for a new one) across the activity being destroyed. */
    private fun rememberChat() {
        val chat = _ui.value.chat
        saved["chat"] = chat?.let { it.id.orEmpty() }
        saved["chat_root"] = chat?.root
    }

    /** The daemon's default agent and model for a new chat in [root]. */
    private fun loadDefaultConfig(root: String) = viewModelScope.launch {
        runCatching { client.request("chat.set_config", JSONObject().put("root", root)) as JSONObject }
            .onSuccess { cfg ->
                _ui.update { s -> s.copy(chat = s.chat?.takeIf { it.id == null && it.root == root }?.copy(config = parseConfig(cfg)) ?: s.chat) }
            }
    }

    private suspend fun reopen(root: String, id: String) {
        runCatching { client.request("chat.open", JSONObject().put("root", root).put("chat", id)) as JSONObject }
            .onSuccess { obj ->
                _ui.update { s ->
                    if (s.chat?.id != id) return@update s
                    s.copy(
                        chat = s.chat.copy(
                            title = obj.optString("title").ifBlank { s.chat.title },
                            config = parseConfig(obj.optJSONObject("config")),
                            messages = parseMessages(obj.optJSONArray("messages")),
                            running = obj.optBoolean("running"),
                            queued = obj.optInt("queued"),
                            approval = parseApproval(obj.optJSONObject("approval")),
                            evidence = obj.optBoolean("evidence"),
                            plan = obj.optBoolean("plan"),
                            context = parseContext(obj.optJSONObject("context")),
                            status = obj.optString("status").takeIf { !obj.isNull("status") && it.isNotEmpty() },
                            loading = false,
                            error = null,
                        )
                    )
                }
                if (obj.optBoolean("running")) watch.follow(root, id, obj.optString("title"), running = true)
            }
            .onFailure { e -> _ui.update { s -> s.copy(chat = s.chat?.copy(loading = false, error = e.message)) } }
    }

    fun closeChat() {
        persistDrafts()
        _ui.update { it.copy(chat = null) }
        rememberChat()
        _ui.value.root?.let { refreshChats(it) }
    }

    fun deleteChat(id: String) = viewModelScope.launch {
        val root = _ui.value.root ?: return@launch
        runCatching { client.request("chat.delete", JSONObject().put("root", root).put("chat", id)) }
            .onFailure { toast(it.message) }
    }

    fun renameChat(title: String) = viewModelScope.launch {
        val chat = _ui.value.chat ?: return@launch
        val id = chat.id ?: return@launch
        runCatching { client.request("chat.rename", JSONObject().put("root", chat.root).put("chat", id).put("title", title)) }
            .onFailure { toast(it.message) }
    }

    // ---- turns -----------------------------------------------------------------------------

    fun send(text: String) {
        val chat = _ui.value.chat ?: return
        val trimmed = text.trim()
        val attachments = chat.attachments
        if ((trimmed.isEmpty() && attachments.isEmpty()) || chat.attaching > 0) return
        val params = JSONObject().put("root", chat.root).put("text", trimmed).put("evidence", chat.evidence).put("plan", chat.plan)
        if (attachments.isNotEmpty()) params.put("attachments", JSONArray(attachments.map { it.toJson() }))
        if (chat.id != null) params.put("chat", chat.id)
        else chat.config?.let { params.put("provider", it.provider).put("model", it.model).put("effort", it.effort) }
        _ui.update { s -> s.copy(chat = s.chat?.copy(sending = true, error = null)) }
        viewModelScope.launch {
            runCatching { client.request("prompt.send", params) as JSONObject }
                .onSuccess { r ->
                    val id = r.optString("chat")
                    val queued = r.optInt("queued")
                    val userIndex = if (r.isNull("user_index")) -1 else r.optInt("user_index", -1)
                    _ui.update { s ->
                        val c = s.chat ?: return@update s
                        if (c.id != null && c.id != id) return@update s
                        // The server announces the prompt (a `message` note) before it answers, so
                        // it is usually here already; add it only when it isn't.
                        val messages = if (queued == 0 && userIndex !in c.messages.indices) {
                            c.messages + Message.User(trimmed, attachments.map { it.label })
                        } else c.messages
                        s.copy(
                            chat = c.copy(
                                id = id,
                                title = r.optString("title").ifBlank { c.title },
                                messages = messages,
                                running = true,
                                queued = queued,
                                sending = false,
                                attachments = c.attachments - attachments.toSet(),
                            )
                        )
                    }
                    watch.follow(chat.root, id, r.optString("title").ifBlank { chat.title }, running = true)
                    if (chat.id == null) {
                        rememberChat()
                        reopen(chat.root, id)
                    }
                }
                .onFailure { e ->
                    // Give the text back: nothing typed on a phone should be lost to a bad connection.
                    val key = draftKey(chat.root, chat.id)
                    if (drafts[key].isNullOrBlank()) drafts[key] = trimmed
                    _ui.update { s ->
                        val c = s.chat?.takeIf { it.root == chat.root && it.id == chat.id } ?: return@update s
                        s.copy(chat = c.copy(sending = false, error = e.message, draftSeed = c.draftSeed + 1))
                    }
                }
        }
    }

    fun setEvidence(on: Boolean) = _ui.update { s -> s.copy(chat = s.chat?.copy(evidence = on)) }

    fun setPlan(on: Boolean) = _ui.update { s -> s.copy(chat = s.chat?.copy(plan = on)) }

    // ---- drafts ----------------------------------------------------------------------------

    private fun draftKey(root: String, id: String?) = "${_ui.value.activeId}|$root|${id.orEmpty()}"

    private fun loadDrafts() {
        val obj = runCatching { JSONObject(prefs.getString("drafts", null) ?: return) }.getOrNull() ?: return
        obj.keys().forEach { drafts[it] = obj.optString(it) }
    }

    fun persistDrafts() {
        val obj = JSONObject()
        drafts.filterValues { it.isNotBlank() }.forEach { (k, v) -> obj.put(k, v) }
        prefs.edit().putString("drafts", obj.toString()).apply()
    }

    fun draftFor(chat: OpenChat): String = drafts[draftKey(chat.root, chat.id)].orEmpty()

    fun setDraft(chat: OpenChat, text: String) {
        val key = draftKey(chat.root, chat.id)
        if (text.isEmpty()) drafts.remove(key) else drafts[key] = text
    }

    // ---- shared from other apps ------------------------------------------------------------

    /** Text and files sent to oxi from another app: into the open chat, else the next one opened. */
    fun share(text: String?, uris: List<Uri>) {
        if (text.isNullOrBlank() && uris.isEmpty()) return
        if (_ui.value.servers.isEmpty()) return toast("Connect to a computer first")
        _ui.update { s ->
            val old = s.share ?: Share()
            s.copy(share = old.copy(text = listOf(old.text, text.orEmpty().trim()).filter { it.isNotBlank() }.joinToString("\n\n"), loading = old.loading + uris.size))
        }
        // Read them now: the permission to is only lent for as long as this screen lives.
        val context = getApplication<Application>()
        uris.forEach { uri ->
            viewModelScope.launch {
                val files = "file_attachments" in _ui.value.features || _ui.value.conn !is ConnState.Connected
                val result = runCatching { Attachments.load(context, uri, files) }
                val added = listOfNotNull(result.getOrNull())
                _ui.update { s ->
                    val share = s.share
                    // Still waiting for a chat, or already handed to the open one.
                    if (share != null) s.copy(share = share.copy(attachments = share.attachments + added, loading = (share.loading - 1).coerceAtLeast(0)))
                    else s.copy(chat = s.chat?.let { it.copy(attachments = it.attachments + added, attaching = (it.attaching - 1).coerceAtLeast(0)) })
                }
                result.exceptionOrNull()?.let { toast(it.message ?: "Can't attach that") }
            }
        }
        deliverShare()
    }

    fun cancelShare() = _ui.update { it.copy(share = null) }

    private fun deliverShare() {
        val share = _ui.value.share ?: return
        val chat = _ui.value.chat ?: return
        if (share.text.isNotBlank()) {
            val key = draftKey(chat.root, chat.id)
            drafts[key] = listOf(drafts[key].orEmpty(), share.text).filter { it.isNotBlank() }.joinToString("\n\n")
        }
        _ui.update { s ->
            val c = s.chat ?: return@update s
            val given = s.share ?: return@update s
            s.copy(
                share = null,
                chat = c.copy(attachments = c.attachments + given.attachments, attaching = c.attaching + given.loading, draftSeed = c.draftSeed + 1),
            )
        }
    }

    // ---- camera ----------------------------------------------------------------------------

    /** Where the camera app should write the next photo. */
    fun newPhoto(): Uri {
        val context = getApplication<Application>()
        val dir = java.io.File(context.cacheDir, "camera").apply { mkdirs() }
        // Photos left behind by a camera that never came back.
        dir.listFiles()?.filter { System.currentTimeMillis() - it.lastModified() > 3600_000L }?.forEach { it.delete() }
        val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmmss", java.util.Locale.US).format(java.util.Date())
        val file = java.io.File(dir, "photo-$stamp.jpg")
        saved["photo"] = file.absolutePath
        return FileProvider.getUriForFile(context, "${context.packageName}.files", file)
    }

    fun photoTaken(ok: Boolean) {
        val context = getApplication<Application>()
        val file = saved.remove<String>("photo")?.let { java.io.File(it) } ?: return
        if (!ok || file.length() == 0L) {
            file.delete()
            return
        }
        attach(listOf(FileProvider.getUriForFile(context, "${context.packageName}.files", file))) { file.delete() }
    }

    // ---- attachments -----------------------------------------------------------------------

    fun attach(uris: List<Uri>, done: () -> Unit = {}) {
        val target = _ui.value.chat ?: return
        if (uris.isEmpty()) return
        val context = getApplication<Application>()
        _ui.update { s -> s.copy(chat = s.chat?.let { it.copy(attaching = it.attaching + uris.size) }) }
        uris.forEach { uri ->
            viewModelScope.launch {
                val files = "file_attachments" in _ui.value.features
                val result = runCatching { Attachments.load(context, uri, files) }
                done()
                _ui.update { s ->
                    val c = s.chat ?: return@update s
                    // Another chat was opened meanwhile.
                    if (c.root != target.root || c.id != target.id) return@update s
                    val added = result.getOrNull()
                    s.copy(chat = c.copy(attaching = (c.attaching - 1).coerceAtLeast(0), attachments = c.attachments + listOfNotNull(added)))
                }
                result.exceptionOrNull()?.let { toast(it.message ?: "Can't attach that") }
            }
        }
    }

    fun removeAttachment(attachment: PendingAttachment) =
        _ui.update { s -> s.copy(chat = s.chat?.let { it.copy(attachments = it.attachments - attachment) }) }

    // ---- images in chats -------------------------------------------------------------------

    private val imageLoads = mutableSetOf<String>()

    fun chatImageKey(chat: String, message: Int, index: Int) = "chat:$chat:$message:$index"
    fun fileImageKey(path: String) = "file:$path"

    /** An image attached to user message [message] of the open chat. */
    fun loadChatImage(message: Int, index: Int) {
        val chat = _ui.value.chat ?: return
        val id = chat.id ?: return
        val params = JSONObject().put("root", chat.root).put("chat", id).put("message", message).put("index", index)
        loadImage(chatImageKey(id, message, index)) { client.request("chat.image", params) as JSONObject }
    }

    /** An evidence capture: a file on the server. */
    fun loadFileImage(path: String) {
        val params = JSONObject().put("path", path).put("max", 20_000_000)
        loadImage(fileImageKey(path)) { client.request("fs.read", params) as JSONObject }
    }

    private fun loadImage(key: String, fetch: suspend () -> JSONObject) {
        if (key in _images.value || !imageLoads.add(key)) return
        viewModelScope.launch {
            val result = runCatching { fetch() }
            imageLoads.remove(key)
            // Not connected: try again when the image is next on screen.
            if (result.exceptionOrNull()?.message.let { it == "Not connected" || it == "Disconnected" }) return@launch
            val bitmap = result.getOrNull()?.optString("data")?.takeIf { it.isNotEmpty() }?.let { data ->
                withContext(Dispatchers.Default) { Attachments.bitmapFromBase64(data)?.asImageBitmap() }
            }
            _images.update { it + (key to bitmap) }
        }
    }

    fun cancel() = viewModelScope.launch {
        val id = _ui.value.chat?.id ?: return@launch
        runCatching { client.request("cancel", JSONObject().put("chat", id)) }.onFailure { toast(it.message) }
    }

    fun respond(decision: String) = viewModelScope.launch {
        val chat = _ui.value.chat ?: return@launch
        val id = chat.id ?: return@launch
        val params = JSONObject().put("chat", id).put("decision", decision)
        if (decision == "allow_prefix") params.put("prefix", chat.approval?.allowPrefix ?: return@launch)
        runCatching { client.request("approval.respond", params) }.onFailure { toast(it.message) }
    }

    /** Answer the agent's question: [content] of the filled form, or null to decline. */
    fun answer(content: JSONObject?) = viewModelScope.launch {
        val id = _ui.value.chat?.id ?: return@launch
        val response = if (content == null) JSONObject().put("action", "decline")
        else JSONObject().put("action", "accept").put("content", content)
        val params = JSONObject().put("chat", id).put("decision", "elicitation").put("response", response)
        runCatching { client.request("approval.respond", params) }.onFailure { toast(it.message) }
    }

    // ---- git -------------------------------------------------------------------------------

    fun openGit() {
        val root = _ui.value.chat?.root ?: _ui.value.root ?: return
        _ui.update { it.copy(git = GitView(root)) }
        git("refresh", "Reading")
    }

    fun closeGit() = _ui.update { it.copy(git = null) }

    fun closeDiff() = _ui.update { s -> s.copy(git = s.git?.let { g -> g.copy(state = g.state?.copy(diff = null)) }) }

    /** Run a git operation on the server; every one answers with the repository's new state. */
    fun git(op: String, label: String, params: JSONObject = JSONObject(), then: (suspend () -> Unit)? = null) {
        val view = _ui.value.git ?: return
        if (view.busy != null) return
        _ui.update { s -> s.copy(git = s.git?.copy(busy = label, error = null)) }
        viewModelScope.launch {
            val result = runCatching { client.request("git", params.put("root", view.root).put("op", op)) as JSONObject }
            _ui.update { s ->
                val g = s.git?.takeIf { it.root == view.root } ?: return@update s
                val state = result.getOrNull()?.let(::parseGit)
                val old = g.state
                // A diff answers with the diff alone; keep the lists already shown.
                val merged = if (state != null && old != null && (op == "diff" || op == "show_commit")) old.copy(diff = state.diff, error = state.error) else state ?: old
                s.copy(git = g.copy(state = merged, busy = null, error = result.exceptionOrNull()?.message ?: state?.error))
            }
            if (result.isSuccess && result.getOrNull()?.isNull("error") != false) then?.invoke()
        }
    }

    private fun paths(files: List<GitFile>) = JSONObject().put("paths", JSONArray(files.map { it.path }))

    fun gitStage(files: List<GitFile>) = git("stage", "Staging", paths(files))

    fun gitUnstage(files: List<GitFile>) = git("unstage", "Unstaging", paths(files))

    /** Commit what is staged; with nothing staged, everything that changed. */
    fun gitCommit(message: String) {
        val state = _ui.value.git?.state ?: return
        val commit = { git("commit", "Committing", JSONObject().put("message", message.trim())) { toast("Committed") } }
        if (state.staged.isEmpty() && state.unstaged.isNotEmpty()) git("stage", "Staging", paths(state.unstaged)) { commit() }
        else commit()
    }

    fun gitWriteMessage() {
        val view = _ui.value.git ?: return
        if (view.generating) return
        _ui.update { s -> s.copy(git = s.git?.copy(generating = true)) }
        viewModelScope.launch {
            val result = runCatching { client.request("git.commit_message", JSONObject().put("root", view.root)) as JSONObject }
            _ui.update { s ->
                val g = s.git ?: return@update s
                val text = result.getOrNull()?.optString("message").orEmpty()
                if (text.isBlank()) g.copy(generating = false).let { s.copy(git = it) }
                else s.copy(git = g.copy(generating = false, generated = text, generatedSeed = g.generatedSeed + 1))
            }
            result.exceptionOrNull()?.let { toast(it.message) }
        }
    }

    fun setProvider(provider: String, model: String? = null) = viewModelScope.launch {
        val chat = _ui.value.chat ?: return@launch
        val params = JSONObject().put("root", chat.root).put("provider", provider)
        if (model != null) params.put("model", model)
        if (chat.id != null) params.put("chat", chat.id)
        runCatching { client.request("chat.set_config", params) as JSONObject }
            .onSuccess { cfg -> _ui.update { s -> s.copy(chat = s.chat?.copy(config = parseConfig(cfg))) } }
            .onFailure { toast(it.message) }
    }

    fun loadModels(provider: String) = viewModelScope.launch {
        val root = _ui.value.chat?.root ?: _ui.value.root ?: return@launch
        runCatching { client.request("models.list", JSONObject().put("root", root).put("provider", provider)) as JSONObject }
            .onSuccess { obj -> _ui.update { it.copy(models = it.models + (provider to parseModels(obj))) } }
            .onFailure { toast(it.message) }
    }

    fun loadQuota(provider: String) = viewModelScope.launch {
        runCatching { client.request("quota.get", JSONObject().put("provider", provider)) as JSONObject }
            .onSuccess { obj -> _ui.update { it.copy(quota = it.quota + (provider to parseQuota(obj))) } }
    }

    // ---- notifications ---------------------------------------------------------------------

    private fun onNote(note: Note) {
        val p = note.params
        val chatId = p.optString("chat")
        when (note.method) {
            "chats_changed" -> if (p.optString("root") == _ui.value.root) _ui.value.root?.let { refreshChats(it) }
            "chat_removed" -> {
                _ui.update { s -> s.copy(chats = s.chats.filterNot { it.id == chatId }, chat = s.chat?.takeIf { it.id != chatId }) }
            }
            "chat_reloaded" -> _ui.value.chat?.takeIf { it.id == chatId }?.let { c ->
                viewModelScope.launch { reopen(c.root, chatId) }
            }
            else -> updateOpenChat(chatId) { c -> applyChatNote(note.method, p, c) }
        }
    }

    private fun applyChatNote(method: String, p: JSONObject, c: OpenChat): OpenChat = when (method) {
        "message" -> {
            val index = p.optInt("index")
            val message = parseMessage(p.getJSONObject("message"))
            when {
                index < c.messages.size -> c.copy(messages = c.messages.toMutableList().also { it[index] = message })
                index == c.messages.size -> c.copy(messages = c.messages + message)
                else -> {
                    // Prompts we didn't see (another client, a queued one): read the chat again.
                    c.id?.let { id -> viewModelScope.launch { reopen(c.root, id) } }
                    c
                }
            }
        }
        "turn" -> c.copy(
            running = p.optBoolean("running").also { running -> if (running) c.id?.let { watch.follow(c.root, it, c.title, running = true) } },
            plan = if (p.has("plan") && p.optBoolean("running") && !p.has("outcome")) p.optBoolean("plan") else c.plan,
            // A turn starting reports the mode it runs in; later notes would undo a choice made
            // on the phone meanwhile for the next prompt.
            evidence = if (p.has("evidence") && p.optBoolean("running") && !p.has("outcome")) p.optBoolean("evidence") else c.evidence,
            queued = p.optInt("queued"),
            error = p.optString("error").takeIf { !p.isNull("error") && it.isNotEmpty() } ?: c.error.takeIf { p.optBoolean("running") },
        )
        "approval" -> c.copy(approval = parseApproval(p.optJSONObject("approval")))
        "status" -> c.copy(status = p.optString("text").takeIf { !p.isNull("text") && it.isNotEmpty() })
        "title" -> c.copy(title = p.optString("title"))
        "config" -> c.copy(config = parseConfig(p.optJSONObject("config")))
        "context" -> c.copy(context = parseContext(p))
        else -> c
    }

    private fun updateOpenChat(id: String, change: (OpenChat) -> OpenChat) {
        if (id.isEmpty()) return
        _ui.update { s ->
            val c = s.chat
            if (c?.id == id) s.copy(chat = change(c)) else s
        }
    }

    fun toast(message: String?) {
        if (message != null) _ui.update { it.copy(toast = message) }
    }

    fun toastShown() = _ui.update { it.copy(toast = null) }
}

package dev.oxi.android

import org.json.JSONArray
import org.json.JSONObject

data class Provider(val id: String, val label: String, val description: String, val acp: Boolean)

data class Workspace(val path: String, val name: String, val modified: Long?, val chats: Int)

data class ChatSummary(
    val id: String,
    val title: String,
    val modified: Long,
    val provider: String?,
    val running: Boolean,
    val approval: Boolean,
)

data class ChatConfig(val provider: String, val label: String, val model: String, val effort: String)

/** [question]: the agent asks something (an ACP elicitation) instead of wanting a tool allowed. */
data class Approval(val tool: String, val summary: String, val allowPrefix: String?, val question: Question? = null)

sealed interface Block {
    data class Thinking(val text: String) : Block
    data class Answer(val text: String) : Block
    data class Tool(
        val id: String?,
        val name: String,
        val kind: String,
        val title: String,
        val status: String,
        val output: String,
        val diff: String?,
        val args: String?,
    ) : Block
}

sealed interface Message {
    /** [images]: indexes into the message's attachments that are images (`chat.image`). */
    data class User(val text: String, val attachments: List<String>, val images: List<Int> = emptyList()) : Message
    data class Assistant(
        val streaming: Boolean,
        val blocks: List<Block>,
        val routeLabel: String?,
        val workedSecs: Long?,
        val evidence: Evidence? = null,
    ) : Message
}

/** What an evidence-mode turn proved (`EvidenceReport` in src/evidence.rs). */
data class Evidence(
    val stale: Boolean,
    val requested: Boolean,
    val submitted: Boolean,
    val summary: String,
    val items: List<EvidenceItem>,
    val claims: List<Claim>,
) {
    val checks get() = items.filter { it.kind == "check" }
    val captures get() = items.filter { (it.kind == "image" || it.kind == "video") && it.image != null }
    val worthShowing get() = items.isNotEmpty() || claims.isNotEmpty() || summary.isNotBlank() || requested

    /** Same rules as the desktop app: a failed cited check contradicts, all-vouched attests. */
    fun status(claim: Claim): ClaimStatus {
        val cited = claim.evidence.mapNotNull { id -> items.firstOrNull { it.id == id.trim() } }
        if (cited.any { it.kind == "check" && !it.passed }) return ClaimStatus.Contradicted
        if (stale) return ClaimStatus.Declared
        val vouched = cited.isNotEmpty() && cited.size == claim.evidence.size &&
            cited.all { if (it.kind == "check") it.passed else it.origin == "oxi" || it.fresh }
        return if (vouched) ClaimStatus.Attested else ClaimStatus.Declared
    }
}

enum class ClaimStatus { Attested, Declared, Contradicted }

data class Claim(val text: String, val evidence: List<String>)

data class EvidenceItem(
    val id: String,
    val kind: String,
    val phase: String,
    val label: String,
    val origin: String,
    val fresh: Boolean,
    /** A still image to show for a capture: the file itself, or a video's preview. */
    val image: String?,
    val command: String?,
    val exitCode: Int?,
    val timedOut: Boolean,
    val output: String,
    val durationMs: Long,
) {
    val passed get() = kind == "check" && exitCode == 0 && !timedOut
}

data class QuotaWindow(val label: String, val usedPct: Double, val resetsIn: Long?)

private fun JSONObject.str(key: String): String? =
    if (isNull(key)) null else optString(key).takeIf { it.isNotEmpty() }

private fun JSONArray?.objects(): List<JSONObject> =
    if (this == null) emptyList() else (0 until length()).mapNotNull { optJSONObject(it) }

fun parseProviders(init: JSONObject): List<Provider> =
    init.optJSONArray("providers").objects().map {
        Provider(it.optString("id"), it.optString("label"), it.optString("description"), it.optBoolean("acp"))
    }

fun parseFeatures(init: JSONObject): Set<String> = init.optJSONArray("features").strings().toSet()

fun parseWorkspaces(arr: JSONArray?): List<Workspace> =
    arr.objects().map {
        Workspace(
            it.optString("path"),
            it.optString("name"),
            if (it.isNull("modified")) null else it.optLong("modified"),
            it.optInt("chats"),
        )
    }

fun parseChats(arr: JSONArray?): List<ChatSummary> =
    arr.objects().map {
        ChatSummary(
            it.optString("id"),
            it.optString("title").ifBlank { "New chat" },
            it.optLong("modified"),
            it.str("provider"),
            it.optBoolean("running"),
            it.optBoolean("approval"),
        )
    }

fun parseConfig(obj: JSONObject?): ChatConfig? = obj?.let {
    ChatConfig(it.optString("provider"), it.optString("label"), it.optString("model"), it.optString("effort"))
}

fun parseContext(obj: JSONObject?): Pair<Long, Long>? =
    obj?.takeIf { it.has("used") && it.optLong("size") > 0 }?.let { it.optLong("used") to it.optLong("size") }

fun parseApproval(obj: JSONObject?): Approval? = obj?.let {
    Approval(it.optString("tool"), it.optString("summary"), it.str("allow_prefix"), it.optJSONObject("elicitation")?.let(::parseQuestion))
}

fun parseMessage(obj: JSONObject): Message {
    if (obj.optString("role") == "user") {
        val attachments = obj.optJSONArray("attachments")
        return Message.User(
            obj.optString("text"),
            if (attachments == null) emptyList() else (0 until attachments.length()).map { attachments.optString(it) },
            obj.optJSONArray("images").objects().map { it.optInt("index") },
        )
    }
    val blocks = obj.optJSONArray("blocks").objects().map { b ->
        when (b.optString("type")) {
            "thinking" -> Block.Thinking(b.optString("text"))
            "tool" -> Block.Tool(
                id = b.str("id"),
                name = b.optString("name"),
                kind = b.optString("kind"),
                title = b.optString("title"),
                status = b.optString("status"),
                output = b.optString("output"),
                diff = b.str("diff"),
                args = b.str("args"),
            )
            else -> Block.Answer(b.optString("text"))
        }
    }
    val route = obj.optJSONObject("route")
    return Message.Assistant(
        streaming = obj.optBoolean("streaming"),
        blocks = blocks,
        routeLabel = route?.let { r ->
            listOfNotNull(r.str("label"), r.str("model")).joinToString(" · ").ifBlank { null }
        },
        workedSecs = if (obj.isNull("worked_secs")) null else obj.optLong("worked_secs"),
        evidence = obj.optJSONObject("evidence")?.let(::parseEvidence),
    )
}

private fun JSONArray?.strings(): List<String> =
    if (this == null) emptyList() else (0 until length()).map { optString(it) }

private val IMAGE_EXTENSIONS = setOf("png", "jpg", "jpeg", "gif", "webp")

private fun isImagePath(path: String?) = path != null && path.substringAfterLast('.').lowercase() in IMAGE_EXTENSIONS

fun parseEvidence(obj: JSONObject): Evidence = Evidence(
    stale = obj.optBoolean("stale"),
    requested = obj.optBoolean("requested"),
    submitted = obj.optBoolean("submitted"),
    summary = obj.optString("summary"),
    items = obj.optJSONArray("items").objects().map {
        val file = it.str("file")
        val preview = it.str("preview")
        EvidenceItem(
            id = it.optString("id"),
            kind = it.optString("kind"),
            phase = it.optString("phase"),
            label = it.optString("label"),
            origin = it.optString("origin"),
            fresh = it.optBoolean("fresh"),
            image = listOf(file, preview).firstOrNull(::isImagePath),
            command = it.str("command"),
            exitCode = if (it.isNull("exit_code")) null else it.optInt("exit_code"),
            timedOut = it.optBoolean("timed_out"),
            output = it.optString("output"),
            durationMs = it.optLong("duration_ms"),
        )
    },
    claims = obj.optJSONArray("claims").objects().map { Claim(it.optString("text"), it.optJSONArray("evidence").strings()) },
)

fun parseMessages(arr: JSONArray?): List<Message> = arr.objects().map(::parseMessage)

fun parseModels(obj: JSONObject?): List<String> {
    val arr = obj?.optJSONArray("models") ?: return emptyList()
    return (0 until arr.length()).mapNotNull { i ->
        when (val m = arr.opt(i)) {
            is String -> m
            is JSONObject -> m.str("id") ?: m.str("model_id") ?: m.str("modelId") ?: m.str("name")
            else -> null
        }
    }
}

fun parseQuota(obj: JSONObject?): List<QuotaWindow> =
    obj?.optJSONArray("windows").objects().map {
        QuotaWindow(
            it.optString("label"),
            it.optDouble("used_pct", 0.0),
            if (it.isNull("resets_in")) null else it.optLong("resets_in"),
        )
    }

// ---- questions (ACP elicitations) -----------------------------------------------------------

enum class FieldKind { Text, Number, Integer, Toggle, Choice, Multi }

data class Option(val value: String, val label: String, val description: String)

data class Field(
    val key: String,
    val title: String,
    val description: String,
    val required: Boolean,
    val kind: FieldKind,
    val options: List<Option> = emptyList(),
    /** A String, a Boolean or a Set<String>, by [kind]. */
    val default: Any? = null,
)

/** What the agent asks (`Elicitation` in src/agent/elicitation.rs): a form, or a page to visit. */
data class Question(val message: String, val url: String?, val fields: List<Field>) {
    fun defaults(): Map<String, Any> = fields.mapNotNull { f -> f.default?.let { f.key to it } }.toMap()

    /** The `content` of an accepted form for [answers], or what is still wrong with them. */
    fun content(answers: Map<String, Any>): Result<JSONObject> {
        val out = JSONObject()
        for (f in fields) {
            val name = f.title.ifBlank { "This field" }
            val value: Any? = when (f.kind) {
                FieldKind.Toggle -> answers[f.key] as? Boolean ?: false
                FieldKind.Text, FieldKind.Choice -> (answers[f.key] as? String)?.takeIf { it.isNotBlank() }
                FieldKind.Number, FieldKind.Integer -> {
                    val typed = (answers[f.key] as? String)?.trim().orEmpty()
                    if (typed.isEmpty()) null else {
                        val number = typed.toDoubleOrNull()?.takeIf { it.isFinite() }
                            ?: return Result.failure(IllegalArgumentException("$name must be a number"))
                        if (f.kind == FieldKind.Integer) {
                            if (number % 1.0 != 0.0) return Result.failure(IllegalArgumentException("$name must be a whole number"))
                            number.toLong()
                        } else number
                    }
                }
                FieldKind.Multi -> (answers[f.key] as? Set<*>)?.takeIf { it.isNotEmpty() }
                    ?.let { picks -> JSONArray(f.options.map { it.value }.filter { it in picks }) }
            }
            if (value == null) {
                if (f.required) return Result.failure(IllegalArgumentException("$name is required"))
            } else out.put(f.key, value)
        }
        return Result.success(out)
    }
}

/** Text order, with runs of digits compared as numbers: `question_2` before `question_10`. */
fun naturalCompare(a: String, b: String): Int {
    var i = 0
    var j = 0
    while (i < a.length && j < b.length) {
        if (a[i].isDigit() && b[j].isDigit()) {
            val si = i
            val sj = j
            while (i < a.length && a[i].isDigit()) i++
            while (j < b.length && b[j].isDigit()) j++
            val x = a.substring(si, i).trimStart('0')
            val y = b.substring(sj, j).trimStart('0')
            if (x.length != y.length) return x.length.compareTo(y.length)
            val c = x.compareTo(y)
            if (c != 0) return c
        } else {
            if (a[i] != b[j]) return a[i].compareTo(b[j])
            i++
            j++
        }
    }
    return (a.length - i).compareTo(b.length - j)
}

private fun parseOptions(schema: JSONObject?): List<Option> {
    if (schema == null) return emptyList()
    (schema.optJSONArray("oneOf") ?: schema.optJSONArray("anyOf"))?.let { items ->
        return items.objects().mapNotNull { item ->
            val value = item.str("const") ?: return@mapNotNull null
            Option(value, item.str("title") ?: value, item.optString("description"))
        }
    }
    val values = schema.optJSONArray("enum") ?: return emptyList()
    val names = schema.optJSONArray("enumNames")
    return (0 until values.length()).mapNotNull { i ->
        val value = values.opt(i) as? String ?: return@mapNotNull null
        Option(value, names?.opt(i) as? String ?: value, "")
    }
}

fun parseQuestion(params: JSONObject): Question {
    val message = params.optString("message")
    if (params.optString("mode") == "url") return Question(message, params.optString("url"), emptyList())
    val schema = params.optJSONObject("requestedSchema")
    val required = schema?.optJSONArray("required").strings().toSet()
    val properties = schema?.optJSONObject("properties")
    val fields = properties?.keys()?.asSequence()?.toList().orEmpty().mapNotNull { key ->
        val s = properties?.optJSONObject(key) ?: return@mapNotNull null
        val base = Field(key, s.optString("title"), s.optString("description"), key in required, FieldKind.Text)
        when (s.optString("type")) {
            "boolean" -> base.copy(kind = FieldKind.Toggle, default = s.optBoolean("default"))
            "number", "integer" -> base.copy(
                kind = if (s.optString("type") == "integer") FieldKind.Integer else FieldKind.Number,
                default = s.opt("default")?.takeIf { it is Number }?.toString(),
            )
            "array" -> base.copy(
                kind = FieldKind.Multi,
                options = parseOptions(s.optJSONObject("items")),
                default = s.optJSONArray("default").strings().toSet().takeIf { it.isNotEmpty() },
            )
            else -> {
                val options = parseOptions(s)
                val default = s.str("default")
                if (options.isEmpty()) base.copy(default = default)
                else base.copy(kind = FieldKind.Choice, options = options, default = default?.takeIf { d -> options.any { it.value == d } })
            }
        }
    }
    return Question(message, null, fields.sortedWith { x, y -> naturalCompare(x.key, y.key) })
}

// ---- the agent's checklist ------------------------------------------------------------------

data class Todo(val text: String, val status: String)

/** The items of a `todo_write` call's arguments, or null when they aren't a checklist. */
fun parseTodos(args: String?): List<Todo>? {
    if (args.isNullOrBlank()) return null
    val obj = runCatching { JSONObject(args) }.getOrNull() ?: return null
    val items = obj.optJSONArray("todos") ?: return null
    return items.objects().mapNotNull { item ->
        val text = item.str("content") ?: item.str("text") ?: item.str("title") ?: return@mapNotNull null
        Todo(text, item.optString("status", "pending"))
    }.takeIf { it.isNotEmpty() }
}

/** The checklist the agent keeps for the prompt being answered: its latest `todo_write`. */
fun latestTodoArgs(messages: List<Message>): String? {
    for (m in messages.asReversed()) {
        if (m is Message.User) return null
        val blocks = (m as Message.Assistant).blocks
        for (b in blocks.asReversed()) {
            if (b is Block.Tool && b.name.lowercase().replace("_", "").contains("todowrite") && b.args != null) return b.args
        }
    }
    return null
}

// ---- git ------------------------------------------------------------------------------------

data class GitFile(val path: String, val status: String)

data class GitCommit(val hash: String, val date: String, val author: String, val message: String)

data class GitDiff(val title: String, val text: String)

/** `git_state_json` in src/headless/mod.rs. */
data class GitState(
    val repo: Boolean,
    val branch: String,
    val ahead: Int,
    val behind: Int,
    val staged: List<GitFile>,
    val unstaged: List<GitFile>,
    val log: List<GitCommit>,
    val diff: GitDiff?,
    val error: String?,
)

fun parseGit(obj: JSONObject): GitState {
    fun files(key: String) = obj.optJSONArray(key).objects().map { GitFile(it.optString("path"), it.optString("status")) }
    return GitState(
        repo = obj.optBoolean("repo"),
        branch = obj.optString("branch"),
        ahead = obj.optInt("ahead"),
        behind = obj.optInt("behind"),
        staged = files("staged"),
        unstaged = files("unstaged"),
        log = obj.optJSONArray("log").objects().map {
            GitCommit(it.optString("hash"), it.optString("date"), it.optString("author"), it.optString("message"))
        },
        diff = obj.optJSONObject("diff")?.let { GitDiff(it.optString("title"), it.optString("text")) },
        error = obj.str("error"),
    )
}

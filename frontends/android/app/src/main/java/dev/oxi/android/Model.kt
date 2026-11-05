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

data class Approval(val tool: String, val summary: String, val allowPrefix: String?)

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

fun parseApproval(obj: JSONObject?): Approval? = obj?.let {
    Approval(it.optString("tool"), it.optString("summary"), it.str("allow_prefix"))
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

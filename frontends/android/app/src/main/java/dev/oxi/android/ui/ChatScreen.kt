package dev.oxi.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowForward
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.ArrowDownward
import androidx.compose.material.icons.rounded.Checklist
import androidx.compose.material.icons.rounded.Commit
import androidx.compose.material.icons.rounded.ContentCopy
import androidx.compose.material.icons.rounded.PhotoCamera
import androidx.compose.material.icons.rounded.Share
import androidx.compose.material.icons.rounded.AttachFile
import androidx.compose.material.icons.rounded.Image
import androidx.compose.material.icons.rounded.Shield
import androidx.compose.material.icons.rounded.ArrowUpward
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.ExpandMore
import androidx.compose.material.icons.rounded.Psychology
import androidx.compose.material.icons.rounded.Stop
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.oxi.android.Approval
import dev.oxi.android.Block
import dev.oxi.android.MainActivity
import dev.oxi.android.Message
import dev.oxi.android.Todo
import dev.oxi.android.latestTodoArgs
import dev.oxi.android.parseTodos
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import dev.oxi.android.OpenChat
import dev.oxi.android.OxiViewModel
import dev.oxi.android.PendingAttachment
import dev.oxi.android.UiState

@Composable
fun ChatScreen(vm: OxiViewModel, state: UiState) {
    val chat = state.chat ?: return
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    // The view model keeps drafts per chat; the seed says it changed one behind our back.
    var draft by remember(chat.root, chat.id, chat.draftSeed) { mutableStateOf(vm.draftFor(chat)) }
    var picker by remember { mutableStateOf(false) }
    var rename by remember { mutableStateOf(false) }
    var zoom by remember { mutableStateOf<ImageBitmap?>(null) }
    val images by vm.images.collectAsState()
    val pickPhotos = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(MAX_PICK)) { vm.attach(it) }
    val pickFiles = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { vm.attach(it) }
    val list = rememberLazyListState()
    // Jump to the end once the transcript has loaded, then follow only from the bottom.
    var opened by remember(chat.id) { mutableStateOf(false) }
    // Keep following the reply while the user is at the bottom.
    val atBottom by remember {
        derivedStateOf {
            val info = list.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull()
            last == null || (last.index >= info.totalItemsCount - 1 && last.offset + last.size <= info.viewportEndOffset + 80)
        }
    }
    // Follow the reply until the user scrolls up, not whenever the end is out of view: the
    // checklist or an approval card appearing shrinks the list without anyone scrolling.
    var follow by remember(chat.id) { mutableStateOf(true) }
    LaunchedEffect(list) {
        snapshotFlow { list.isScrollInProgress to atBottom }.collect { (scrolling, bottom) -> if (scrolling) follow = bottom }
    }
    val contentKey = chat.messages.size to (chat.messages.lastOrNull()?.let(::contentSize) ?: 0)
    LaunchedEffect(contentKey, chat.approval, chat.messages.lastOrNull()?.let(::latestTodoArgsOf)) {
        if ((follow || !opened) && chat.messages.isNotEmpty()) {
            list.scrollToItem(chat.messages.lastIndex, Int.MAX_VALUE / 2)
            opened = true
        }
    }
    LaunchedEffect(chat.id == null) {
        if (chat.id == null && chat.messages.isEmpty()) list.scrollToItem(0)
    }

    Column(Modifier.fillMaxSize().background(Oxi.bg).imePadding()) {
        TopBar(
            title = chat.title.ifBlank { "New chat" },
            subtitle = listOfNotNull(chat.root.substringAfterLast('/'), chat.contextPct?.let { "$it% context" }).joinToString(" · "),
            onBack = { vm.closeChat() },
            actions = {
                IconButton(onClick = { vm.openGit() }) { Icon(Icons.Rounded.Commit, "Source control", tint = Oxi.muted) }
                if (chat.id != null) IconButton(onClick = { rename = true }) { Icon(Icons.Rounded.Edit, "Rename", tint = Oxi.muted) }
                Box(Modifier.padding(end = 14.dp)) { ConnDot(state.conn) }
            },
        )
        Box(Modifier.weight(1f).fillMaxWidth()) {
            if (chat.loading && chat.messages.isEmpty()) {
                CircularProgressIndicator(Modifier.align(Alignment.Center), color = Oxi.copper)
            } else if (chat.messages.isEmpty()) {
                EmptyChat(chat, Modifier.align(Alignment.Center))
            }
            LazyColumn(
                state = list,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 12.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                itemsIndexed(chat.messages) { index, message ->
                    when (message) {
                        is Message.User -> UserBubble(message, index, chat.id, vm, images) { zoom = it }
                        is Message.Assistant -> AssistantMessage(message, live = chat.running && index == chat.messages.lastIndex, vm, images) { zoom = it }
                    }
                }
                if (chat.status != null || chat.error != null) {
                    item {
                        Text(
                            chat.error ?: chat.status ?: "",
                            color = if (chat.error != null) Oxi.red else Oxi.yellow,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
            if (!atBottom && chat.messages.isNotEmpty()) {
                Box(
                    Modifier
                        .align(Alignment.BottomCenter)
                        .padding(bottom = 10.dp)
                        .size(36.dp)
                        .clip(CircleShape)
                        .background(Oxi.raised)
                        .border(1.dp, Oxi.line, CircleShape)
                        .clickable { follow = true; scope.launch { list.scrollToItem(chat.messages.lastIndex, Int.MAX_VALUE / 2) } },
                    contentAlignment = Alignment.Center,
                ) { Icon(Icons.Rounded.ArrowDownward, "Jump to the end", tint = Oxi.text, modifier = Modifier.size(18.dp)) }
            }
        }
        val todoArgs = remember(chat.messages) { latestTodoArgs(chat.messages) }
        val todos = remember(todoArgs) { parseTodos(todoArgs) }
        if (todos != null && (chat.running || todos.any { it.status != "completed" })) TodoStrip(todos)
        chat.approval?.let { a -> if (a.question != null) QuestionCard(a.question, vm) else ApprovalCard(a, vm) }
        Composer(
            chat = chat,
            draft = draft,
            onDraft = { draft = it; vm.setDraft(chat, it) },
            onSend = {
                vm.setDraft(chat, "")
                vm.send(draft)
                draft = ""
                (context as? MainActivity)?.askForNotifications()
            },
            onStop = { vm.cancel() },
            onPicker = { picker = true },
            onCamera = { (context as? MainActivity)?.takePhoto() },
            onPlan = { vm.setPlan(!chat.plan) },
            onPhotos = { pickPhotos.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
            onFiles = { pickFiles.launch(arrayOf("*/*")) },
            onRemove = { vm.removeAttachment(it) },
            onEvidence = { vm.setEvidence(!chat.evidence) },
        )
    }
    zoom?.let { ImageZoom(it) { zoom = null } }
    if (picker) ProviderSheet(vm, state, chat) { picker = false }
    if (rename) {
        var title by remember { mutableStateOf(chat.title) }
        AlertDialog(
            onDismissRequest = { rename = false },
            containerColor = Oxi.surface,
            title = { Text("Rename chat") },
            text = { OutlinedTextField(title, { title = it }, singleLine = true) },
            confirmButton = { TextButton(onClick = { vm.renameChat(title); rename = false }) { Text("Save") } },
            dismissButton = { TextButton(onClick = { rename = false }) { Text("Cancel") } },
        )
    }
}

/** The checklist a reply carries, to follow the list as it appears. */
private fun latestTodoArgsOf(m: Message): String? = latestTodoArgs(listOf(m))

private fun contentSize(m: Message): Int = when (m) {
    is Message.User -> m.text.length
    is Message.Assistant -> m.blocks.sumOf {
        when (it) {
            is Block.Answer -> it.text.length
            is Block.Thinking -> it.text.length
            is Block.Tool -> it.output.length + it.status.length + 1
        }
    } + (if (m.streaming) 0 else 1)
}

@Composable
private fun EmptyChat(chat: OpenChat, modifier: Modifier) {
    Column(modifier.padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Text("What should we work on?", style = MaterialTheme.typography.titleMedium)
        Text(
            listOfNotNull(chat.config?.label, chat.root.substringAfterLast('/')).joinToString(" in "),
            color = Oxi.muted,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 6.dp),
        )
    }
}

/** Photos that can be picked at once. */
private const val MAX_PICK = 8

@Composable
private fun UserBubble(
    m: Message.User,
    index: Int,
    chatId: String?,
    vm: OxiViewModel,
    images: Map<String, ImageBitmap?>,
    onZoom: (ImageBitmap) -> Unit,
) {
    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
        Column(
            Modifier
                .widthIn(max = 320.dp)
                .clip(RoundedCornerShape(18.dp, 18.dp, 4.dp, 18.dp))
                .background(Oxi.raised)
                .padding(horizontal = 14.dp, vertical = 10.dp)
        ) {
            if (chatId != null && m.images.isNotEmpty()) {
                Row(
                    Modifier.horizontalScroll(androidx.compose.foundation.rememberScrollState()).padding(bottom = if (m.text.isBlank()) 0.dp else 8.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    m.images.forEach { i ->
                        val key = vm.chatImageKey(chatId, index, i)
                        LaunchedEffect(key) { vm.loadChatImage(index, i) }
                        ImageThumb(images[key], loaded = key in images, size = 120, onZoom = onZoom)
                    }
                }
            }
            if (m.text.isNotBlank()) SelectionContainer { Text(m.text, style = MaterialTheme.typography.bodyLarge) }
            // Images show above; list the rest (text files, or images not saved yet).
            val others = m.attachments.filterIndexed { i, _ -> chatId == null || i !in m.images }
            if (others.isNotEmpty()) {
                Text(others.joinToString(" · "), style = MaterialTheme.typography.bodySmall, color = Oxi.muted, modifier = Modifier.padding(top = 4.dp))
            }
        }
    }
}

@Composable
private fun AssistantMessage(
    m: Message.Assistant,
    live: Boolean,
    vm: OxiViewModel,
    images: Map<String, ImageBitmap?>,
    onZoom: (ImageBitmap) -> Unit,
) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        // Consecutive tool calls read as one group.
        var i = 0
        val blocks = m.blocks
        while (i < blocks.size) {
            when (val b = blocks[i]) {
                is Block.Answer -> if (b.text.isNotBlank()) Markdown(b.text)
                is Block.Thinking -> if (b.text.isNotBlank()) ThinkingRow(b.text, live && i == blocks.lastIndex)
                is Block.Tool -> {
                    val group = mutableListOf<Block.Tool>()
                    while (i < blocks.size && blocks[i] is Block.Tool) { group += blocks[i] as Block.Tool; i++ }
                    ToolGroup(group)
                    continue
                }
            }
            i++
        }
        m.evidence?.takeIf { it.worthShowing && !m.streaming }?.let { EvidenceCard(it, vm, images, onZoom) }
        if (live && m.streaming) {
            WorkingDots()
        } else if (!m.streaming) {
            val answer = remember(m) { m.blocks.filterIsInstance<Block.Answer>().joinToString("\n\n") { it.text.trim() }.trim() }
            if (answer.isNotEmpty() || m.routeLabel != null || m.workedSecs != null) MessageFooter(m, answer)
        }
    }
}

/** Who answered and how long it took, with copy and share for the reply's text. */
@Composable
private fun MessageFooter(m: Message.Assistant, answer: String) {
    val context = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(1500); copied = false } }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            listOfNotNull(m.routeLabel, m.workedSecs?.let { formatSecs(it) }).joinToString(" · "),
            style = MaterialTheme.typography.labelSmall,
            color = Oxi.faint,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (answer.isNotEmpty()) {
            IconButton(
                onClick = {
                    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("reply", answer))
                    copied = true
                },
                modifier = Modifier.size(32.dp),
            ) {
                Icon(
                    if (copied) Icons.Rounded.Check else Icons.Rounded.ContentCopy,
                    "Copy reply",
                    tint = if (copied) Oxi.green else Oxi.faint,
                    modifier = Modifier.size(15.dp),
                )
            }
            IconButton(
                onClick = {
                    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, answer)
                    runCatching { context.startActivity(Intent.createChooser(send, null)) }
                },
                modifier = Modifier.size(32.dp),
            ) { Icon(Icons.Rounded.Share, "Share reply", tint = Oxi.faint, modifier = Modifier.size(15.dp)) }
        }
    }
}

/** The agent's checklist, above the composer: where it is, and the whole list on a tap. */
@Composable
private fun TodoStrip(todos: List<Todo>) {
    var open by remember { mutableStateOf(false) }
    val done = todos.count { it.status == "completed" }
    val current = todos.firstOrNull { it.status == "in_progress" } ?: todos.firstOrNull { it.status != "completed" }
    Column(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 4.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(Oxi.surface)
            .border(1.dp, Oxi.line, RoundedCornerShape(14.dp))
            .clickable { open = !open }
            .padding(horizontal = 14.dp, vertical = 9.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("$done/${todos.size}", style = MonoSmall, color = Oxi.copper)
            Text(
                if (open) "Plan" else current?.text ?: "All done",
                style = MaterialTheme.typography.bodySmall,
                color = if (open) Oxi.muted else Oxi.text,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).padding(horizontal = 10.dp),
            )
            Icon(Icons.Rounded.ExpandMore, null, tint = Oxi.faint, modifier = Modifier.size(16.dp).rotate(if (open) 0f else 180f))
        }
        AnimatedVisibility(open) {
            Column(
                Modifier.padding(top = 8.dp).heightIn(max = 260.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(7.dp),
            ) {
                todos.forEach { todo ->
                    Row(verticalAlignment = Alignment.Top) {
                        Box(Modifier.padding(top = 2.dp).size(16.dp), contentAlignment = Alignment.Center) {
                            when (todo.status) {
                                "completed" -> Icon(Icons.Rounded.Check, "Done", tint = Oxi.green, modifier = Modifier.size(15.dp))
                                "in_progress" -> CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp, color = Oxi.copper)
                                else -> Box(Modifier.size(11.dp).border(1.5.dp, Oxi.faint, CircleShape))
                            }
                        }
                        Text(
                            todo.text,
                            style = MaterialTheme.typography.bodySmall,
                            color = if (todo.status == "completed") Oxi.faint else Oxi.text,
                            modifier = Modifier.padding(start = 9.dp),
                        )
                    }
                }
            }
        }
    }
}

private fun formatSecs(s: Long): String = when {
    s < 60 -> "${s}s"
    s < 3600 -> "${s / 60}m ${s % 60}s"
    else -> "${s / 3600}h ${(s % 3600) / 60}m"
}

@Composable
private fun WorkingDots() {
    val t = rememberInfiniteTransition(label = "dots")
    Row(horizontalArrangement = Arrangement.spacedBy(5.dp), modifier = Modifier.padding(vertical = 4.dp)) {
        repeat(3) { n ->
            val a by t.animateFloat(
                0.25f, 1f,
                infiniteRepeatable(tween(600, delayMillis = n * 180), RepeatMode.Reverse),
                label = "dot$n",
            )
            Box(Modifier.size(6.dp).alpha(a).clip(CircleShape).background(Oxi.copper))
        }
    }
}

@Composable
private fun ThinkingRow(text: String, live: Boolean) {
    var open by remember { mutableStateOf(false) }
    Column(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).clickable { open = !open }.padding(vertical = 4.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Rounded.Psychology, null, tint = Oxi.faint, modifier = Modifier.size(16.dp))
            Text(
                if (live) "Thinking…" else "Thought",
                style = MaterialTheme.typography.bodySmall,
                color = Oxi.muted,
                modifier = Modifier.padding(start = 6.dp),
            )
            Icon(Icons.Rounded.ExpandMore, null, tint = Oxi.faint, modifier = Modifier.size(16.dp).rotate(if (open) 180f else 0f))
        }
        AnimatedVisibility(open) {
            Text(text.trim(), style = MaterialTheme.typography.bodySmall, color = Oxi.muted, modifier = Modifier.padding(start = 22.dp, top = 4.dp))
        }
    }
}

@Composable
private fun ToolGroup(tools: List<Block.Tool>) {
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .border(1.dp, Oxi.line, RoundedCornerShape(12.dp))
            .background(Oxi.surface)
            .padding(vertical = 4.dp)
    ) {
        tools.forEach { ToolRow(it) }
    }
}

private fun toolVerb(t: Block.Tool): String {
    val n = t.name.lowercase()
    return when {
        t.kind == "read" || n == "read" -> "Read"
        t.kind == "edit" || n == "edit" || n == "write" || n == "multiedit" -> "Edit"
        t.kind == "execute" || n == "bash" -> "Run"
        t.kind == "search" || n == "grep" || n == "glob" || n == "find" -> "Search"
        t.kind == "fetch" || n.contains("fetch") || n.contains("web") -> "Fetch"
        n == "todo_write" || n.contains("todo") -> "Plan"
        else -> t.name.replaceFirstChar { it.uppercase() }.ifBlank { "Tool" }
    }
}

@Composable
private fun ToolRow(t: Block.Tool) {
    var open by remember { mutableStateOf(false) }
    val hasBody = t.output.isNotBlank() || !t.diff.isNullOrBlank()
    Column(Modifier.fillMaxWidth()) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable(enabled = hasBody) { open = !open }
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            StatusMark(t.status)
            Text(
                buildAnnotatedString {
                    withStyle(SpanStyle(fontWeight = FontWeight.Medium, color = Oxi.text)) { append(toolVerb(t)) }
                    if (t.title.isNotBlank()) {
                        append("  ")
                        withStyle(SpanStyle(fontFamily = Oxi.mono, fontSize = 12.5.sp, color = Oxi.muted)) {
                            append(t.title.lineSequence().first())
                        }
                    }
                },
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).padding(start = 10.dp),
            )
            if (hasBody) Icon(Icons.Rounded.ExpandMore, null, tint = Oxi.faint, modifier = Modifier.size(18.dp).rotate(if (open) 180f else 0f))
        }
        AnimatedVisibility(open) {
            Column(Modifier.padding(start = 8.dp, end = 8.dp, bottom = 8.dp)) {
                val diff = t.diff
                if (!diff.isNullOrBlank()) DiffBox(diff) else CodeBox(t.output.trimEnd().takeLast(4000))
            }
        }
    }
}

@Composable
private fun StatusMark(status: String) {
    Box(Modifier.size(16.dp), contentAlignment = Alignment.Center) {
        when (status) {
            "pending", "in_progress" -> CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp, color = Oxi.copper)
            "failed" -> Icon(Icons.Rounded.Close, "Failed", tint = Oxi.red, modifier = Modifier.size(15.dp))
            "interrupted" -> Box(Modifier.size(7.dp).clip(CircleShape).background(Oxi.faint))
            else -> Icon(Icons.Rounded.Check, "Done", tint = Oxi.green, modifier = Modifier.size(15.dp))
        }
    }
}

@Composable
fun DiffBox(diff: String, maxLines: Int = 400) {
    val text: AnnotatedString = remember(diff, maxLines) {
        buildAnnotatedString {
            diff.trimEnd().lineSequence().take(maxLines).forEach { line ->
                val color = when {
                    line.startsWith("+++") || line.startsWith("---") -> Oxi.faint
                    line.startsWith("+") -> Oxi.green
                    line.startsWith("-") -> Oxi.red
                    line.startsWith("@@") -> Color(0xFF7FA7D9)
                    else -> Oxi.muted
                }
                if (length > 0) append('\n')
                withStyle(SpanStyle(color = color)) { append(line) }
            }
        }
    }
    Box(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).background(Oxi.codeBg)
            .horizontalScrollCompat().padding(horizontal = 12.dp, vertical = 10.dp)
    ) { Text(text, style = MonoSmall) }
}

@Composable
private fun Modifier.horizontalScrollCompat(): Modifier =
    this.then(Modifier.horizontalScroll(androidx.compose.foundation.rememberScrollState()))

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ApprovalCard(a: Approval, vm: OxiViewModel) {
    Column(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(Oxi.raised)
            .border(1.dp, Oxi.yellow.copy(alpha = 0.35f), RoundedCornerShape(16.dp))
            .padding(14.dp)
    ) {
        Text("Allow ${a.tool}?", style = MaterialTheme.typography.titleSmall, color = Oxi.yellow)
        Text(
            a.summary,
            style = MonoSmall,
            color = Oxi.text,
            maxLines = 6,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = 6.dp, bottom = 10.dp),
        )
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Pill("Allow", primary = true) { vm.respond("approve") }
            if (a.allowPrefix != null) Pill("Always `${a.allowPrefix}`") { vm.respond("allow_prefix") }
            else if (a.tool != "bash") Pill("Always here") { vm.respond("allow_tool") }
            Pill("Allow all this turn") { vm.respond("approve_rest") }
            Pill("Deny", danger = true) { vm.respond("deny") }
        }
    }
}

@Composable
fun Pill(text: String, primary: Boolean = false, danger: Boolean = false, onClick: () -> Unit) {
    val bg = when {
        primary -> Oxi.copper
        else -> Oxi.surface
    }
    val fg = when {
        primary -> Color(0xFF1A0E05)
        danger -> Oxi.red
        else -> Oxi.text
    }
    Box(
        Modifier
            .clip(RoundedCornerShape(50))
            .background(bg)
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 8.dp)
    ) {
        Text(text, color = fg, style = MaterialTheme.typography.labelLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
private fun Composer(
    chat: OpenChat,
    draft: String,
    onDraft: (String) -> Unit,
    onSend: () -> Unit,
    onStop: () -> Unit,
    onPicker: () -> Unit,
    onCamera: () -> Unit,
    onPlan: () -> Unit,
    onPhotos: () -> Unit,
    onFiles: () -> Unit,
    onRemove: (PendingAttachment) -> Unit,
    onEvidence: () -> Unit,
) {
    var attachMenu by remember { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .navigationBarsPadding()
            .padding(start = 12.dp, end = 12.dp, bottom = 10.dp, top = 4.dp)
            .clip(RoundedCornerShape(22.dp))
            .background(Oxi.surface)
            .border(1.dp, Oxi.line, RoundedCornerShape(22.dp))
            .padding(start = 16.dp, end = 8.dp, top = 12.dp, bottom = 8.dp)
    ) {
        if (chat.attachments.isNotEmpty() || chat.attaching > 0) {
            Row(
                Modifier.fillMaxWidth().horizontalScroll(androidx.compose.foundation.rememberScrollState()).padding(end = 8.dp, bottom = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                chat.attachments.forEach { AttachmentChip(it) { onRemove(it) } }
                if (chat.attaching > 0) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = Oxi.copper)
            }
        }
        BasicTextField(
            value = draft,
            onValueChange = onDraft,
            textStyle = MaterialTheme.typography.bodyLarge.copy(color = Oxi.text),
            cursorBrush = SolidColor(Oxi.copper),
            modifier = Modifier.fillMaxWidth().heightIn(min = 24.dp, max = 180.dp).padding(end = 8.dp),
            decorationBox = { inner ->
                if (draft.isEmpty()) {
                    Text(
                        when {
                            chat.running -> "Queue a follow-up…"
                            chat.plan -> "Ask oxi for a plan…"
                            else -> "Ask oxi…"
                        },
                        style = MaterialTheme.typography.bodyLarge,
                        color = Oxi.faint,
                    )
                }
                inner()
            },
        )
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 6.dp)) {
            Box {
                IconButton(onClick = { attachMenu = true }, modifier = Modifier.size(34.dp)) {
                    Icon(Icons.Rounded.Add, "Attach", tint = Oxi.muted)
                }
                DropdownMenu(expanded = attachMenu, onDismissRequest = { attachMenu = false }, containerColor = Oxi.raised) {
                    DropdownMenuItem(
                        text = { Text("Camera") },
                        leadingIcon = { Icon(Icons.Rounded.PhotoCamera, null, tint = Oxi.muted) },
                        onClick = { attachMenu = false; onCamera() },
                    )
                    DropdownMenuItem(
                        text = { Text("Photos") },
                        leadingIcon = { Icon(Icons.Rounded.Image, null, tint = Oxi.muted) },
                        onClick = { attachMenu = false; onPhotos() },
                    )
                    DropdownMenuItem(
                        text = { Text("Files") },
                        leadingIcon = { Icon(Icons.Rounded.AttachFile, null, tint = Oxi.muted) },
                        onClick = { attachMenu = false; onFiles() },
                    )
                }
            }
            IconButton(onClick = onEvidence, modifier = Modifier.size(34.dp)) {
                Icon(
                    Icons.Rounded.Shield,
                    if (chat.evidence) "Evidence mode on" else "Evidence mode off",
                    tint = if (chat.evidence) Oxi.green else Oxi.faint,
                )
            }
            IconButton(onClick = onPlan, modifier = Modifier.size(34.dp)) {
                Icon(
                    Icons.Rounded.Checklist,
                    if (chat.plan) "Plan mode on" else "Plan mode off",
                    tint = if (chat.plan) Oxi.copper else Oxi.faint,
                )
            }
            Row(
                Modifier
                    .weight(1f)
                    .clip(RoundedCornerShape(50))
                    .clickable(onClick = onPicker)
                    .padding(horizontal = 8.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    chat.config?.let { c -> listOf(c.label, c.model).filter { it.isNotBlank() }.joinToString(" · ") } ?: "…",
                    style = MaterialTheme.typography.labelLarge,
                    color = Oxi.muted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                Icon(Icons.Rounded.ExpandMore, null, tint = Oxi.faint, modifier = Modifier.size(16.dp))
            }
            if (chat.queued > 0) {
                Text("${chat.queued} queued", style = MaterialTheme.typography.labelSmall, color = Oxi.faint, maxLines = 1, modifier = Modifier.padding(start = 4.dp))
            }
            Spacer(Modifier.width(6.dp))
            if (chat.running && draft.isBlank() && chat.attachments.isEmpty()) {
                RoundButton(Icons.Rounded.Stop, "Stop", Oxi.raised, Oxi.text, onStop)
            } else {
                val enabled = (draft.isNotBlank() || chat.attachments.isNotEmpty()) && !chat.sending && chat.attaching == 0
                RoundButton(
                    if (chat.running) Icons.AutoMirrored.Rounded.ArrowForward else Icons.Rounded.ArrowUpward,
                    "Send",
                    if (enabled) Oxi.copper else Oxi.raised,
                    if (enabled) Color(0xFF1A0E05) else Oxi.faint,
                    onClick = { if (enabled) onSend() },
                )
            }
        }
    }
}

@Composable
private fun AttachmentChip(a: PendingAttachment, onRemove: () -> Unit) {
    Row(
        Modifier
            .clip(RoundedCornerShape(10.dp))
            .background(Oxi.raised)
            .padding(start = if (a is PendingAttachment.Image) 4.dp else 10.dp, end = 2.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (a is PendingAttachment.Image) {
            val thumb = remember(a) { a.thumb.asImageBitmap() }
            androidx.compose.foundation.Image(
                thumb, a.name,
                contentScale = androidx.compose.ui.layout.ContentScale.Crop,
                modifier = Modifier.size(40.dp).clip(RoundedCornerShape(7.dp)),
            )
        } else {
            Icon(Icons.Rounded.AttachFile, null, tint = Oxi.muted, modifier = Modifier.size(16.dp))
            Column(Modifier.widthIn(max = 160.dp).padding(start = 4.dp)) {
                Text(a.name, style = MaterialTheme.typography.labelMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (a is PendingAttachment.File) {
                    Text(dev.oxi.android.Attachments.sizeLabel(a.bytes.size), style = MaterialTheme.typography.labelSmall, color = Oxi.faint)
                }
            }
        }
        IconButton(onClick = onRemove, modifier = Modifier.size(28.dp)) {
            Icon(Icons.Rounded.Close, "Remove", tint = Oxi.muted, modifier = Modifier.size(16.dp))
        }
    }
}

@Composable
private fun RoundButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    bg: Color,
    fg: Color,
    onClick: () -> Unit,
) {
    Box(
        Modifier.size(38.dp).clip(CircleShape).background(bg).clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) { Icon(icon, label, tint = fg, modifier = Modifier.size(20.dp)) }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ProviderSheet(vm: OxiViewModel, state: UiState, chat: OpenChat, onDismiss: () -> Unit) {
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val current = chat.config?.provider
    LaunchedEffect(current) {
        if (current != null) {
            if (state.models[current] == null) vm.loadModels(current)
            vm.loadQuota(current)
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet, containerColor = Oxi.surface) {
        LazyColumn(Modifier.fillMaxWidth().padding(bottom = 24.dp)) {
            item { SectionLabelPadded("Agent") }
            itemsIndexed(state.providers) { _, p ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable { vm.setProvider(p.id) }
                        .padding(horizontal = 20.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(p.label, style = MaterialTheme.typography.bodyLarge)
                        Text(p.description, style = MaterialTheme.typography.bodySmall, color = Oxi.faint)
                    }
                    if (p.id == current) Icon(Icons.Rounded.Check, null, tint = Oxi.copper)
                }
            }
            val quota = current?.let { state.quota[it] }.orEmpty()
            if (quota.isNotEmpty()) {
                item { SectionLabelPadded("Usage") }
                item {
                    Column(Modifier.padding(horizontal = 20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        quota.forEach { w ->
                            val color = when { w.usedPct >= 90 -> Oxi.red; w.usedPct >= 70 -> Oxi.yellow; else -> Oxi.copper }
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(w.label, style = MaterialTheme.typography.bodySmall, color = Oxi.muted, modifier = Modifier.widthIn(min = 64.dp))
                                Box(Modifier.weight(1f).padding(horizontal = 10.dp).heightIn(min = 6.dp).clip(RoundedCornerShape(3.dp)).background(Oxi.raised)) {
                                    Box(Modifier.fillMaxWidth((w.usedPct / 100.0).toFloat().coerceIn(0f, 1f)).heightIn(min = 6.dp).background(color))
                                }
                                Text(
                                    "${w.usedPct.toInt()}%" + (w.resetsIn?.let { " · ↻" + formatSecs(it) } ?: ""),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = Oxi.muted,
                                )
                            }
                        }
                    }
                }
            }
            val models = current?.let { state.models[it] }
            item { SectionLabelPadded("Model") }
            if (models == null) {
                item {
                    Row(Modifier.padding(horizontal = 20.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp, color = Oxi.copper)
                        Text("Asking the agent…", color = Oxi.faint, modifier = Modifier.padding(start = 10.dp))
                    }
                }
            } else {
                itemsIndexed(models) { _, model ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clickable { vm.setProvider(current, model); onDismiss() }
                            .padding(horizontal = 20.dp, vertical = 11.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(model, style = MaterialTheme.typography.bodyMedium.copy(fontFamily = Oxi.mono), modifier = Modifier.weight(1f))
                        if (model == chat.config?.model) Icon(Icons.Rounded.Check, null, tint = Oxi.copper)
                    }
                }
            }
        }
    }
}

@Composable
private fun SectionLabelPadded(text: String) {
    Box(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) { SectionLabel(text) }
}

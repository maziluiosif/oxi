package dev.oxi.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.ArrowDownward
import androidx.compose.material.icons.rounded.ArrowUpward
import androidx.compose.material.icons.rounded.AutoAwesome
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material.icons.rounded.Remove
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.oxi.android.GitFile
import dev.oxi.android.GitView
import dev.oxi.android.OxiViewModel
import org.json.JSONObject

/** Source control for the open workspace: what changed, the diffs, commit, pull and push. */
@Composable
fun GitSheet(vm: OxiViewModel, view: GitView) {
    val state = view.state
    val diff = state?.diff
    var confirmPush by remember { mutableStateOf(false) }
    val back = { if (diff != null) vm.closeDiff() else vm.closeGit() }
    Dialog(onDismissRequest = back, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Column(Modifier.fillMaxSize().background(Oxi.bg)) {
            TopBar(
                title = diff?.title?.substringAfterLast('/')?.ifBlank { null } ?: state?.branch?.ifBlank { null } ?: "Source control",
                subtitle = if (diff != null) diff.title else listOfNotNull(
                    view.root.substringAfterLast('/'),
                    state?.takeIf { it.ahead > 0 }?.let { "↑${it.ahead}" },
                    state?.takeIf { it.behind > 0 }?.let { "↓${it.behind}" },
                ).joinToString(" · "),
                onBack = back,
                actions = {
                    if (diff == null && state?.repo == true) {
                        IconButton(onClick = { vm.git("pull", "Pulling") }) { Icon(Icons.Rounded.ArrowDownward, "Pull", tint = Oxi.muted) }
                        IconButton(onClick = { confirmPush = true }) {
                            Icon(Icons.Rounded.ArrowUpward, "Push", tint = if (state.ahead > 0) Oxi.copper else Oxi.muted)
                        }
                    }
                    if (diff == null) IconButton(onClick = { vm.git("refresh", "Reading") }) { Icon(Icons.Rounded.Refresh, "Refresh", tint = Oxi.muted) }
                },
            )
            if (view.busy != null) {
                LinearProgressIndicator(Modifier.fillMaxWidth(), color = Oxi.copper, trackColor = Oxi.raised)
            }
            view.error?.let {
                Text(it.trim(), color = Oxi.red, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
            }
            when {
                diff != null -> Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp)) {
                    if (diff.text.isBlank()) Text("Nothing to show", color = Oxi.muted) else DiffBox(diff.text, maxLines = 3000)
                }
                state == null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    if (view.busy != null) Text("${view.busy}…", color = Oxi.muted)
                }
                !state.repo -> Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) {
                    Text("This folder isn't a git repository", color = Oxi.muted)
                }
                else -> Changes(vm, view)
            }
        }
    }
    if (confirmPush) {
        AlertDialog(
            onDismissRequest = { confirmPush = false },
            containerColor = Oxi.surface,
            title = { Text("Push ${state?.branch.orEmpty()}?") },
            text = {
                Text(
                    state?.takeIf { it.ahead > 0 }?.let { "${it.ahead} commit${if (it.ahead == 1) "" else "s"} go to the remote." }
                        ?: "Sends this branch to its remote.",
                    color = Oxi.muted,
                )
            },
            confirmButton = { TextButton(onClick = { confirmPush = false; vm.git("push", "Pushing") { vm.toast("Pushed") } }) { Text("Push") } },
            dismissButton = { TextButton(onClick = { confirmPush = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun Changes(vm: OxiViewModel, view: GitView) {
    val state = view.state ?: return
    var message by remember { mutableStateOf("") }
    LaunchedEffect(view.generatedSeed) { if (view.generatedSeed > 0) message = view.generated }
    // A commit empties the staged list: its message is used up.
    val changed = state.staged.size + state.unstaged.size
    LaunchedEffect(state.log.firstOrNull()?.hash) { if (state.staged.isEmpty()) message = "" }
    fun show(file: GitFile, staged: Boolean) =
        vm.git("diff", "Reading", JSONObject().put("path", file.path).put("staged", staged))
    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 32.dp),
    ) {
        if (changed > 0) {
            item {
                Column(
                    Modifier
                        .fillMaxWidth()
                        .padding(vertical = 4.dp)
                        .clip(RoundedCornerShape(16.dp))
                        .background(Oxi.surface)
                        .border(1.dp, Oxi.line, RoundedCornerShape(16.dp))
                        .padding(start = 14.dp, end = 8.dp, top = 12.dp, bottom = 8.dp)
                ) {
                    BasicTextField(
                        value = message,
                        onValueChange = { message = it },
                        textStyle = MaterialTheme.typography.bodyMedium.copy(color = Oxi.text),
                        cursorBrush = SolidColor(Oxi.copper),
                        modifier = Modifier.fillMaxWidth().heightIn(min = 22.dp, max = 140.dp).padding(end = 6.dp),
                        decorationBox = { inner ->
                            if (message.isEmpty()) Text("Commit message", style = MaterialTheme.typography.bodyMedium, color = Oxi.faint)
                            inner()
                        },
                    )
                    Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Row(
                            Modifier
                                .clip(RoundedCornerShape(50))
                                .clickable(enabled = !view.generating) { vm.gitWriteMessage() }
                                .padding(horizontal = 6.dp, vertical = 6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            if (view.generating) CircularProgressIndicator(Modifier.size(15.dp), strokeWidth = 1.5.dp, color = Oxi.copper)
                            else Icon(Icons.Rounded.AutoAwesome, null, tint = Oxi.copper, modifier = Modifier.size(16.dp))
                            Text(
                                if (view.generating) "Writing…" else "Write it for me",
                                style = MaterialTheme.typography.labelLarge,
                                color = Oxi.muted,
                                modifier = Modifier.padding(start = 6.dp),
                            )
                        }
                        Spacer(Modifier.weight(1f))
                        val ready = message.isNotBlank() && view.busy == null
                        Box(Modifier.padding(end = 4.dp)) {
                            Pill(if (state.staged.isEmpty()) "Commit all" else "Commit", primary = ready) { if (ready) vm.gitCommit(message) }
                        }
                    }
                }
            }
        }
        if (state.staged.isNotEmpty()) {
            item { ListHeader("Staged", state.staged.size, "Unstage all") { vm.gitUnstage(state.staged) } }
            items(state.staged, key = { "s:" + it.path }) { file ->
                FileRow(file, Icons.Rounded.Remove, "Unstage", onOpen = { show(file, true) }) { vm.gitUnstage(listOf(file)) }
            }
        }
        if (state.unstaged.isNotEmpty()) {
            item { ListHeader("Changes", state.unstaged.size, "Stage all") { vm.gitStage(state.unstaged) } }
            items(state.unstaged, key = { "u:" + it.path }) { file ->
                FileRow(file, Icons.Rounded.Add, "Stage", onOpen = { show(file, false) }) { vm.gitStage(listOf(file)) }
            }
        }
        if (changed == 0) {
            item { Text("Nothing to commit", color = Oxi.muted, modifier = Modifier.padding(horizontal = 8.dp, vertical = 20.dp)) }
        }
        if (state.log.isNotEmpty()) {
            item { ListHeader("Recent commits", null, null) {} }
            items(state.log.take(30), key = { "c:" + it.hash }) { commit ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(10.dp))
                        .clickable { vm.git("show_commit", "Reading", JSONObject().put("hash", commit.hash)) }
                        .padding(horizontal = 8.dp, vertical = 9.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(commit.hash.take(7), style = MonoSmall, color = Oxi.faint)
                    Column(Modifier.weight(1f).padding(start = 12.dp)) {
                        Text(commit.message.lineSequence().firstOrNull().orEmpty(), style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(
                            listOf(commit.author, commit.date).filter { it.isNotBlank() }.joinToString(" · "),
                            style = MaterialTheme.typography.bodySmall,
                            color = Oxi.faint,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ListHeader(text: String, count: Int?, action: String?, onAction: () -> Unit) {
    Row(Modifier.fillMaxWidth().padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f)) { SectionLabel(if (count != null) "$text · $count" else text) }
        if (action != null) TextButton(onClick = onAction) { Text(action, color = Oxi.copper, style = MaterialTheme.typography.labelLarge) }
    }
}

@Composable
private fun FileRow(
    file: GitFile,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    onOpen: () -> Unit,
    onAction: () -> Unit,
) {
    val color = when (file.status) {
        "A", "?" -> Oxi.green
        "D" -> Oxi.red
        "U" -> Oxi.red
        else -> Oxi.yellow
    }
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).clickable(onClick = onOpen).padding(start = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(if (file.status == "?") "U" else file.status, style = MonoSmall, color = color, fontWeight = FontWeight.Bold, modifier = Modifier.width(16.dp))
        val name = file.path.trimEnd('/').substringAfterLast('/')
        val dir = file.path.trimEnd('/').substringBeforeLast('/', "")
        Text(
            buildAnnotatedString {
                append(name)
                if (dir.isNotEmpty()) withStyle(SpanStyle(color = Oxi.faint, fontSize = MonoSmall.fontSize)) { append("  $dir") }
            },
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f).padding(start = 8.dp),
        )
        IconButton(onClick = onAction) { Icon(icon, label, tint = Oxi.muted, modifier = Modifier.size(20.dp)) }
    }
}

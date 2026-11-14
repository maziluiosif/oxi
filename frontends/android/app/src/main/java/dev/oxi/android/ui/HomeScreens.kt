package dev.oxi.android.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.ChatBubbleOutline
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Commit
import androidx.compose.material.icons.rounded.Share
import androidx.compose.material.icons.rounded.CreateNewFolder
import androidx.compose.material.icons.rounded.Folder
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.LinkOff
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.PanTool
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.oxi.android.ChatSummary
import dev.oxi.android.ConnState
import dev.oxi.android.Endpoint
import dev.oxi.android.OxiViewModel
import dev.oxi.android.R
import dev.oxi.android.UiState
import dev.oxi.android.Workspace

fun ago(secs: Long?): String {
    if (secs == null || secs <= 0) return ""
    val d = System.currentTimeMillis() / 1000 - secs
    return when {
        d < 60 -> "now"
        d < 3600 -> "${d / 60}m"
        d < 86400 -> "${d / 3600}h"
        d < 86400 * 7 -> "${d / 86400}d"
        else -> "${d / (86400 * 7)}w"
    }
}

@Composable
fun ConnDot(conn: ConnState) {
    val color = when (conn) {
        is ConnState.Connected -> Oxi.green
        ConnState.Connecting -> Oxi.yellow
        is ConnState.Failed -> Oxi.red
        ConnState.Idle -> Oxi.faint
    }
    Box(Modifier.size(8.dp).clip(CircleShape).background(color))
}

@Composable
fun TopBar(
    title: String,
    subtitle: String? = null,
    onBack: (() -> Unit)? = null,
    actions: @Composable () -> Unit = {},
    leading: @Composable (() -> Unit)? = null,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .statusBarsPadding()
            .heightIn(min = 60.dp)
            .padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (onBack != null) {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Rounded.ArrowBack, "Back") }
        } else {
            Spacer(Modifier.width(12.dp))
        }
        leading?.invoke()
        Column(Modifier.weight(1f).padding(horizontal = 4.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (!subtitle.isNullOrBlank()) {
                Text(subtitle, style = MaterialTheme.typography.bodySmall, color = Oxi.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        actions()
    }
}

// ---- workspaces -----------------------------------------------------------------------------

@Composable
fun WorkspacesScreen(vm: OxiViewModel, state: UiState) {
    var servers by remember { mutableStateOf(false) }
    var browse by remember { mutableStateOf(false) }
    Scaffold(
        containerColor = Oxi.bg,
        topBar = {
            Row(
                Modifier.fillMaxWidth().statusBarsPadding().heightIn(min = 60.dp).padding(horizontal = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.weight(1f)) { ServerTitle(state) { servers = true } }
                IconButton(onClick = { vm.refreshWorkspaces() }) { Icon(Icons.Rounded.Refresh, "Refresh") }
            }
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = { browse = true },
                icon = { Icon(Icons.Rounded.CreateNewFolder, null) },
                text = { Text("Open folder") },
                containerColor = Oxi.raised,
                contentColor = Oxi.text,
            )
        },
    ) { pad ->
        if (!state.workspacesLoaded) {
            Box(Modifier.fillMaxSize().padding(pad), contentAlignment = Alignment.Center) {
                if (state.conn is ConnState.Failed) {
                    Column(Modifier.padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                        Text("Can't reach ${state.active?.name ?: "the server"}", style = MaterialTheme.typography.titleMedium)
                        Text(
                            (state.conn as ConnState.Failed).message,
                            color = Oxi.muted,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(top = 6.dp, bottom = 10.dp),
                        )
                        Text(
                            "Check that Tailscale is on, here and on the computer, and that its oxi server runs.",
                            color = Oxi.faint,
                            style = MaterialTheme.typography.bodySmall,
                        )
                        TextButton(onClick = { servers = true }) { Text("Switch server") }
                    }
                } else {
                    CircularProgressIndicator(color = Oxi.copper)
                }
            }
        } else {
            LazyColumn(
                Modifier.fillMaxSize().padding(pad),
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 96.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                item { UpdateBanner(vm, state) }
                item { ShareBanner(vm, state, "Pick a project, then a chat") }
                item { SectionLabel("Projects") }
                items(state.workspaces, key = { it.path }) { ws -> WorkspaceRow(ws, state.home) { vm.openWorkspace(ws.path) } }
                if (state.workspaces.isEmpty()) {
                    item { Text("No projects with chats yet. Open a folder to start.", color = Oxi.muted, modifier = Modifier.padding(8.dp)) }
                }
            }
        }
    }
    if (servers) ServersSheet(vm, state) { servers = false }
    if (browse) {
        FolderPicker(vm, start = state.home ?: "~", onDismiss = { browse = false }) { path ->
            browse = false
            vm.openWorkspace(path)
        }
    }
}

/** Something shared from another app is waiting for the chat it should go to. */
@Composable
fun ShareBanner(vm: OxiViewModel, state: UiState, hint: String) {
    val share = state.share ?: return
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(Oxi.copperDim)
            .padding(start = 16.dp, end = 4.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Rounded.Share, null, tint = Oxi.copper, modifier = Modifier.size(20.dp))
        Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
            Text(
                if (share.count == 1) "1 item to share" else "${share.count} items to share",
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium,
            )
            Text(hint, style = MaterialTheme.typography.bodySmall, color = Oxi.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        IconButton(onClick = { vm.cancelShare() }) { Icon(Icons.Rounded.Close, "Don't share", tint = Oxi.muted) }
    }
}

@Composable
fun SectionLabel(text: String) {
    Text(
        text.uppercase(),
        style = MaterialTheme.typography.labelSmall.copy(letterSpacing = 1.2.sp),
        color = Oxi.faint,
        modifier = Modifier.padding(start = 4.dp, top = 8.dp, bottom = 2.dp),
    )
}

@Composable
private fun WorkspaceRow(ws: Workspace, home: String?, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(Oxi.surface)
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier.size(38.dp).clip(RoundedCornerShape(10.dp)).background(Oxi.copperDim),
            contentAlignment = Alignment.Center,
        ) { Icon(Icons.Rounded.Folder, null, tint = Oxi.copper, modifier = Modifier.size(20.dp)) }
        Column(Modifier.weight(1f).padding(start = 14.dp)) {
            Text(ws.name, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val shown = if (home != null && ws.path.startsWith(home)) "~" + ws.path.removePrefix(home) else ws.path
            Text(shown, style = MaterialTheme.typography.bodySmall, color = Oxi.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Column(horizontalAlignment = Alignment.End) {
            Text(ago(ws.modified), style = MaterialTheme.typography.bodySmall, color = Oxi.faint)
            if (ws.chats > 0) Text("${ws.chats} chats", style = MaterialTheme.typography.bodySmall, color = Oxi.faint)
        }
    }
}

@Composable
fun FolderPicker(vm: OxiViewModel, start: String, onDismiss: () -> Unit, onPick: (String) -> Unit) {
    var path by remember { mutableStateOf(start) }
    var parent by remember { mutableStateOf<String?>(null) }
    var dirs by remember { mutableStateOf<List<String>?>(null) }
    LaunchedEffect(path) {
        dirs = null
        vm.listFolder(path) { p, up, list -> path = p; parent = up; dirs = list }
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = Oxi.surface,
        title = { Text(path.substringAfterLast('/').ifBlank { path }, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        text = {
            Column(Modifier.heightIn(max = 420.dp)) {
                Text(path, style = MonoSmall, color = Oxi.faint, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Spacer(Modifier.height(8.dp))
                val list = dirs
                if (list == null) {
                    Box(Modifier.fillMaxWidth().height(120.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Oxi.copper) }
                } else {
                    LazyColumn {
                        parent?.let { up ->
                            item { FolderRow("..", Icons.Rounded.FolderOpen) { path = up } }
                        }
                        items(list) { name -> FolderRow(name, Icons.Rounded.Folder) { path = "$path/$name".replace("//", "/") } }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = { onPick(path) }) { Text("Open here") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun FolderRow(name: String, icon: androidx.compose.ui.graphics.vector.ImageVector, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).clickable(onClick = onClick).padding(vertical = 10.dp, horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, tint = Oxi.muted, modifier = Modifier.size(18.dp))
        Text(name, modifier = Modifier.padding(start = 12.dp), maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

// ---- chats ----------------------------------------------------------------------------------

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun ChatsScreen(vm: OxiViewModel, state: UiState) {
    val root = state.root ?: return
    var confirmDelete by remember { mutableStateOf<ChatSummary?>(null) }
    Scaffold(
        containerColor = Oxi.bg,
        topBar = {
            TopBar(
                title = root.substringAfterLast('/'),
                subtitle = root.let { r -> state.home?.let { if (r.startsWith(it)) "~" + r.removePrefix(it) else r } ?: r },
                onBack = { vm.openWorkspace(null) },
                actions = {
                    IconButton(onClick = { vm.openGit() }) { Icon(Icons.Rounded.Commit, "Source control", tint = Oxi.muted) }
                    Box(Modifier.padding(end = 14.dp)) { ConnDot(state.conn) }
                },
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = { vm.newChat() },
                icon = { Icon(Icons.Rounded.Add, null) },
                text = { Text("New chat") },
                containerColor = Oxi.copper,
                contentColor = Color(0xFF1A0E05),
            )
        },
    ) { pad ->
        LazyColumn(
            Modifier.fillMaxSize().padding(pad),
            contentPadding = PaddingValues(start = 12.dp, end = 12.dp, bottom = 96.dp),
        ) {
            item { ShareBanner(vm, state, "Open a chat or start a new one") }
            if (state.chats.isEmpty() && !state.chatsLoading) {
                item {
                    Column(Modifier.fillMaxWidth().padding(top = 80.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                        Icon(Icons.Rounded.ChatBubbleOutline, null, tint = Oxi.faint, modifier = Modifier.size(40.dp))
                        Text("No chats here yet", color = Oxi.muted, modifier = Modifier.padding(top = 12.dp))
                    }
                }
            }
            items(state.chats, key = { it.id }) { chat ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(12.dp))
                        .combinedClickable(onClick = { vm.openChat(chat.id) }, onLongClick = { confirmDelete = chat })
                        .padding(horizontal = 12.dp, vertical = 13.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(chat.title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        val provider = state.providers.firstOrNull { it.id == chat.provider }?.label ?: chat.provider
                        Text(
                            listOfNotNull(provider, ago(chat.modified).ifBlank { null }).joinToString(" · "),
                            style = MaterialTheme.typography.bodySmall,
                            color = Oxi.faint,
                        )
                    }
                    when {
                        chat.approval -> Icon(Icons.Rounded.PanTool, "Needs approval", tint = Oxi.yellow, modifier = Modifier.size(18.dp))
                        chat.running -> CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp, color = Oxi.copper)
                    }
                }
            }
        }
    }
    confirmDelete?.let { chat ->
        AlertDialog(
            onDismissRequest = { confirmDelete = null },
            containerColor = Oxi.surface,
            title = { Text("Delete chat?") },
            text = { Text(chat.title, color = Oxi.muted) },
            confirmButton = { TextButton(onClick = { vm.deleteChat(chat.id); confirmDelete = null }) { Text("Delete", color = Oxi.red) } },
            dismissButton = { TextButton(onClick = { confirmDelete = null }) { Text("Cancel") } },
        )
    }
}

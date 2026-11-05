package dev.oxi.android.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Info
import androidx.compose.material.icons.rounded.Shield
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
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
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.oxi.android.ClaimStatus
import dev.oxi.android.Evidence
import dev.oxi.android.EvidenceItem
import dev.oxi.android.OxiViewModel

/** The proof under a reply in evidence mode, like the desktop app's report. */
@Composable
fun EvidenceCard(report: Evidence, vm: OxiViewModel, images: Map<String, ImageBitmap?>, onZoom: (ImageBitmap) -> Unit) {
    val accent = if (report.stale) Oxi.yellow else Oxi.green
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .border(1.dp, accent.copy(alpha = 0.3f), RoundedCornerShape(12.dp))
            .background(accent.copy(alpha = 0.04f))
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Rounded.Shield, null, tint = Oxi.green, modifier = Modifier.size(15.dp))
            Text("Evidence", style = MaterialTheme.typography.titleSmall, color = Oxi.green, modifier = Modifier.padding(start = 6.dp))
            if (report.stale) Tag("stale")
            if (report.requested && !report.submitted) Tag("not submitted")
        }
        if (report.summary.isNotBlank()) {
            SelectionContainer { Text(report.summary, style = MaterialTheme.typography.bodyMedium, color = Oxi.muted) }
        }
        report.claims.forEach { claim ->
            val status = report.status(claim)
            Row(verticalAlignment = Alignment.Top) {
                Icon(
                    when (status) {
                        ClaimStatus.Attested -> Icons.Rounded.Check
                        ClaimStatus.Contradicted -> Icons.Rounded.Close
                        ClaimStatus.Declared -> Icons.Rounded.Info
                    },
                    status.name,
                    tint = when (status) {
                        ClaimStatus.Attested -> Oxi.green
                        ClaimStatus.Contradicted -> Oxi.red
                        ClaimStatus.Declared -> Oxi.faint
                    },
                    modifier = Modifier.padding(top = 2.dp).size(15.dp),
                )
                SelectionContainer(Modifier.padding(start = 8.dp)) { Text(claim.text, style = MaterialTheme.typography.bodyMedium) }
            }
        }
        val captures = report.captures
        if (captures.isNotEmpty()) {
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                captures.forEach { item -> Capture(item, vm, images, onZoom) }
            }
        }
        report.checks.forEach { CheckRow(it) }
    }
}

@Composable
private fun Tag(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = Oxi.yellow,
        modifier = Modifier
            .padding(start = 8.dp)
            .clip(RoundedCornerShape(5.dp))
            .background(Oxi.yellow.copy(alpha = 0.12f))
            .padding(horizontal = 6.dp, vertical = 1.dp),
    )
}

@Composable
private fun Capture(item: EvidenceItem, vm: OxiViewModel, images: Map<String, ImageBitmap?>, onZoom: (ImageBitmap) -> Unit) {
    val path = item.image ?: return
    val key = vm.fileImageKey(path)
    LaunchedEffect(key) { vm.loadFileImage(path) }
    Column(Modifier.width(150.dp)) {
        ImageThumb(images[key], loaded = key in images, size = 150, onZoom = onZoom)
        Text(
            listOf(item.phase, item.label).filter { it.isNotBlank() }.joinToString(" · "),
            style = MaterialTheme.typography.labelSmall,
            color = Oxi.muted,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = 4.dp),
        )
    }
}

@Composable
private fun CheckRow(check: EvidenceItem) {
    var open by remember { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(Oxi.surface)
    ) {
        Row(
            Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                if (check.passed) Icons.Rounded.Check else Icons.Rounded.Close,
                if (check.passed) "Passed" else "Failed",
                tint = if (check.passed) Oxi.green else Oxi.red,
                modifier = Modifier.size(15.dp),
            )
            Text(
                check.label.ifBlank { check.command.orEmpty() },
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).padding(start = 8.dp),
            )
            Text(
                if (check.timedOut) "timed out" else "%.1fs".format(check.durationMs / 1000.0),
                style = MaterialTheme.typography.labelSmall,
                color = Oxi.faint,
            )
        }
        AnimatedVisibility(open) {
            Box(Modifier.padding(start = 6.dp, end = 6.dp, bottom = 6.dp)) {
                CodeBox(listOfNotNull(check.command?.let { "$ $it" }, check.output.trimEnd().takeLast(4000)).joinToString("\n"))
            }
        }
    }
}

/** A square image preview; [loaded] with a null [image] means it can't be shown. */
@Composable
fun ImageThumb(image: ImageBitmap?, loaded: Boolean, size: Int, onZoom: (ImageBitmap) -> Unit) {
    Box(
        Modifier
            .size(size.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(Oxi.raised)
            .let { m -> if (image != null) m.clickable { onZoom(image) } else m },
        contentAlignment = Alignment.Center,
    ) {
        when {
            image != null -> Image(image, null, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
            loaded -> Text("No preview", style = MaterialTheme.typography.labelSmall, color = Oxi.faint)
            else -> CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = Oxi.copper)
        }
    }
}

/** Full screen; a tap closes it. */
@Composable
fun ImageZoom(image: ImageBitmap, onDismiss: () -> Unit) {
    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(
            Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.92f)).clickable(onClick = onDismiss),
            contentAlignment = Alignment.Center,
        ) {
            Image(image, null, contentScale = ContentScale.Fit, modifier = Modifier.fillMaxWidth())
        }
    }
}

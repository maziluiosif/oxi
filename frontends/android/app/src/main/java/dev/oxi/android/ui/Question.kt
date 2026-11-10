package dev.oxi.android.ui

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.HelpOutline
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.oxi.android.Field
import dev.oxi.android.FieldKind
import dev.oxi.android.OxiViewModel
import dev.oxi.android.Question

/** The agent asks instead of acting: pick, type, send. In the place of the approval card. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun QuestionCard(q: Question, vm: OxiViewModel) {
    val context = LocalContext.current
    var answers by remember(q) { mutableStateOf(q.defaults()) }
    var problem by remember(q) { mutableStateOf<String?>(null) }
    val url = q.url?.trim()?.takeIf { it.startsWith("https://", true) || it.startsWith("http://", true) }
    Column(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(Oxi.raised)
            .border(1.dp, Oxi.copper.copy(alpha = 0.4f), RoundedCornerShape(16.dp))
            .padding(14.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.AutoMirrored.Rounded.HelpOutline, null, tint = Oxi.copper, modifier = Modifier.size(16.dp))
            Text("oxi asks", style = MaterialTheme.typography.titleSmall, color = Oxi.copper, modifier = Modifier.padding(start = 6.dp))
        }
        Column(
            Modifier
                .padding(top = 8.dp)
                .heightIn(max = (LocalConfiguration.current.screenHeightDp * 0.45f).dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            if (q.message.isNotBlank()) Text(q.message, style = MaterialTheme.typography.bodyMedium)
            if (q.url != null) Text(q.url, style = MonoSmall, color = Oxi.muted)
            q.fields.forEach { field ->
                FieldView(field, answers[field.key]) { value ->
                    answers = if (value == null) answers - field.key else answers + (field.key to value)
                    problem = null
                }
            }
        }
        problem?.let { Text(it, color = Oxi.red, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(top = 8.dp)) }
        FlowRow(
            Modifier.padding(top = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (url != null) {
                Pill("Open page", primary = true) {
                    runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
                        .onFailure { vm.toast("No app can open that page") }
                }
            }
            Pill(if (q.url != null) "Done" else "Send", primary = url == null) {
                q.content(answers).fold(onSuccess = { vm.answer(it) }, onFailure = { problem = it.message })
            }
            Pill("Decline", danger = true) { vm.answer(null) }
        }
    }
}

@Composable
private fun FieldView(field: Field, value: Any?, onChange: (Any?) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (field.kind == FieldKind.Toggle) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) { FieldTitle(field) }
                Switch(value as? Boolean ?: false, onCheckedChange = { onChange(it) })
            }
            return@Column
        }
        FieldTitle(field)
        when (field.kind) {
            FieldKind.Choice -> field.options.forEach { option ->
                val picked = value == option.value
                OptionRow(option.label, option.description, picked, round = true) { onChange(if (picked) null else option.value) }
            }
            FieldKind.Multi -> {
                val picks = (value as? Set<*>).orEmpty().filterIsInstance<String>().toSet()
                field.options.forEach { option ->
                    val picked = option.value in picks
                    OptionRow(option.label, option.description, picked, round = false) {
                        onChange((if (picked) picks - option.value else picks + option.value).takeIf { it.isNotEmpty() })
                    }
                }
            }
            else -> {
                val text = value as? String ?: ""
                BasicTextField(
                    value = text,
                    onValueChange = { onChange(it.takeIf { t -> t.isNotEmpty() }) },
                    textStyle = MaterialTheme.typography.bodyMedium.copy(color = Oxi.text),
                    cursorBrush = SolidColor(Oxi.copper),
                    keyboardOptions = KeyboardOptions(
                        keyboardType = if (field.kind == FieldKind.Text) KeyboardType.Text else KeyboardType.Decimal,
                    ),
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(10.dp))
                        .background(Oxi.surface)
                        .border(1.dp, Oxi.line, RoundedCornerShape(10.dp))
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    decorationBox = { inner ->
                        if (text.isEmpty()) Text("Type an answer", style = MaterialTheme.typography.bodyMedium, color = Oxi.faint)
                        inner()
                    },
                )
            }
        }
    }
}

@Composable
private fun FieldTitle(field: Field) {
    val title = field.title.ifBlank { field.description }
    if (title.isNotBlank()) {
        Text(
            if (field.required) "$title *" else title,
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.Medium,
        )
    }
    if (field.title.isNotBlank() && field.description.isNotBlank()) {
        Text(field.description, style = MaterialTheme.typography.bodySmall, color = Oxi.muted)
    }
}

@Composable
private fun OptionRow(label: String, description: String, picked: Boolean, round: Boolean, onClick: () -> Unit) {
    val shape = if (round) CircleShape else RoundedCornerShape(5.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(if (picked) Oxi.copperDim else Oxi.surface)
            .border(1.dp, if (picked) Oxi.copper.copy(alpha = 0.6f) else Oxi.line, RoundedCornerShape(10.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier.size(18.dp).clip(shape).background(if (picked) Oxi.copper else Color.Transparent)
                .border(1.5.dp, if (picked) Oxi.copper else Oxi.faint, shape),
            contentAlignment = Alignment.Center,
        ) { if (picked) Icon(Icons.Rounded.Check, "Picked", tint = Color(0xFF1A0E05), modifier = Modifier.size(13.dp)) }
        Column(Modifier.padding(start = 12.dp)) {
            Text(label, style = MaterialTheme.typography.bodyMedium)
            if (description.isNotBlank()) Text(description, style = MaterialTheme.typography.bodySmall, color = Oxi.muted)
        }
    }
}

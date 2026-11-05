package dev.oxi.android

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Base64
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction

/** Something picked in the composer, waiting to go out with the next prompt. */
sealed interface PendingAttachment {
    val name: String

    data class Image(override val name: String, val mime: String, val bytes: ByteArray, val thumb: Bitmap) : PendingAttachment
    data class Text(override val name: String, val text: String) : PendingAttachment
    /** Any other file (PDF, Office...): the server saves it in the workspace and reads its text. */
    data class File(override val name: String, val bytes: ByteArray) : PendingAttachment

    /** The shape `prompt.send` takes (`attachment_param`, `file_attachment` in src/headless/mod.rs). */
    fun toJson(): JSONObject = when (this) {
        is Image -> JSONObject().put("mime", mime).put("data", Base64.encodeToString(bytes, Base64.NO_WRAP))
        is Text -> JSONObject().put("name", name).put("text", text)
        is File -> JSONObject().put("name", name).put("file", Base64.encodeToString(bytes, Base64.NO_WRAP))
    }

    /** How the prompt lists it before the server's copy arrives (as `message_json` does). */
    val label: String get() = when (this) {
        is Image -> "image ($mime)"
        is Text, is File -> name
    }
}

class AttachmentError(message: String) : Exception(message)

object Attachments {
    /** Longest edge of an image sent to the agent: plenty for a model, small over mobile data. */
    private const val MAX_EDGE = 2048
    /** Images up to this size that already fit go out untouched. */
    private const val KEEP_BYTES = 2_500_000
    private const val MAX_TEXT_BYTES = 1024 * 1024
    /** `MAX_FILE_ATTACHMENT` in src/headless/mod.rs. */
    private const val MAX_FILE_BYTES = 30 * 1024 * 1024
    private val PASSTHROUGH = setOf("image/png", "image/jpeg", "image/webp", "image/gif")

    /** [files]: the server takes any file (`file_attachments`); older ones only images and text. */
    suspend fun load(context: Context, uri: Uri, files: Boolean): PendingAttachment = withContext(Dispatchers.IO) {
        val resolver = context.contentResolver
        val name = displayName(context, uri) ?: uri.lastPathSegment?.substringAfterLast('/') ?: "attachment"
        val mime = resolver.getType(uri).orEmpty()
        if (mime.startsWith("image/")) {
            // One Android can't decode (SVG, RAW) still goes as a file where the server takes them.
            val image = runCatching { image(context, uri, name, mime) }
            if (image.isSuccess || !files) return@withContext image.getOrThrow()
        }
        val limit = if (files) MAX_FILE_BYTES else MAX_TEXT_BYTES
        val bytes = resolver.openInputStream(uri)?.use { input ->
            val out = ByteArrayOutputStream()
            val buf = ByteArray(64 * 1024)
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                out.write(buf, 0, n)
                if (out.size() > limit) throw AttachmentError("$name is larger than ${limit / (1024 * 1024)} MB")
            }
            out.toByteArray()
        } ?: throw AttachmentError("Can't read $name")
        // Small text files go in the prompt as they are; the rest is saved on the server.
        val text = if (bytes.size <= MAX_TEXT_BYTES) decodeText(bytes) else null
        when {
            text != null -> PendingAttachment.Text(name, text)
            files -> PendingAttachment.File(name, bytes)
            else -> throw AttachmentError("Update oxi on the server to attach $name: this one only takes images and text files")
        }
    }

    fun sizeLabel(bytes: Int): String = when {
        bytes < 1024 -> "$bytes B"
        bytes < 1024 * 1024 -> "${bytes / 1024} KB"
        else -> "%.1f MB".format(bytes / (1024.0 * 1024.0))
    }

    private fun image(context: Context, uri: Uri, name: String, mime: String): PendingAttachment.Image {
        val resolver = context.contentResolver
        val original = resolver.openInputStream(uri)?.use { it.readBytes() } ?: throw AttachmentError("Can't read $name")
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(original, 0, original.size, bounds)
        val fits = bounds.outWidth in 1..MAX_EDGE && bounds.outHeight in 1..MAX_EDGE
        // HEIC and other formats a model may not take are re-encoded too.
        val (bytes, sentMime) = if (fits && original.size <= KEEP_BYTES && mime in PASSTHROUGH) {
            original to mime
        } else {
            val bitmap = decode(context, uri, MAX_EDGE) ?: throw AttachmentError("Can't read $name as an image")
            val out = ByteArrayOutputStream()
            bitmap.compress(Bitmap.CompressFormat.JPEG, 85, out)
            out.toByteArray() to "image/jpeg"
        }
        val thumb = decode(context, uri, 256) ?: throw AttachmentError("Can't read $name as an image")
        return PendingAttachment.Image(name, sentMime, bytes, thumb)
    }

    /** Decoded, rotated as shot, and scaled down to fit [maxEdge]. */
    private fun decode(context: Context, uri: Uri, maxEdge: Int): Bitmap? = runCatching {
        val source = ImageDecoder.createSource(context.contentResolver, uri)
        ImageDecoder.decodeBitmap(source, fit(maxEdge))
    }.getOrNull()

    /** A bitmap from base64 image data (`chat.image`, `fs.read`), scaled down to fit [maxEdge]. */
    fun bitmapFromBase64(data: String, maxEdge: Int = 1600): Bitmap? = runCatching {
        val bytes = Base64.decode(data, Base64.DEFAULT)
        ImageDecoder.decodeBitmap(ImageDecoder.createSource(ByteBuffer.wrap(bytes)), fit(maxEdge))
    }.getOrNull()

    private fun fit(maxEdge: Int) = ImageDecoder.OnHeaderDecodedListener { decoder, info, _ ->
        val (w, h) = info.size.width to info.size.height
        val scale = maxEdge.toFloat() / maxOf(w, h)
        if (scale < 1f) decoder.setTargetSize((w * scale).toInt().coerceAtLeast(1), (h * scale).toInt().coerceAtLeast(1))
        decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
    }

    private fun displayName(context: Context, uri: Uri): String? = runCatching {
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }
    }.getOrNull()

    /** UTF-8 without NUL bytes, or null for binary files. */
    private fun decodeText(bytes: ByteArray): String? {
        if (bytes.any { it == 0.toByte() }) return null
        return try {
            Charsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(bytes))
                .toString()
                .removePrefix("﻿")
        } catch (e: CharacterCodingException) {
            null
        }
    }
}

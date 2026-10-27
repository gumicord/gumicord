package dev.gumicord.app

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.net.Uri
import android.provider.MediaStore
import android.util.Base64
import java.io.ByteArrayOutputStream
import java.io.ByteArrayInputStream
import java.nio.ByteBuffer

/**
 * Clipboard helper for Android.
 * Called from Rust via JNI to implement clipboard operations.
 */
class ClipboardHelper(private val activity: Activity) {

    private val clipboardManager: ClipboardManager =
        activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager

    /**
     * Sets plain text on the clipboard.
     */
    fun setText(text: String) {
        val clip = ClipData.newPlainText("gumicord", text)
        clipboardManager.primaryClip = clip
    }

    /**
     * Gets plain text from the clipboard.
     */
    fun getText(): String? {
        val clip = clipboardManager.primaryClip
        if (clip == null || clip.itemCount == 0) return null
        val item = clip.getItemAt(0)
        return item.text?.toString()
    }

    /**
     * Sets an image on the clipboard.
     * The pixel data is RGBA bytes in a direct ByteBuffer.
     */
    fun setImage(pixels: ByteBuffer, width: Int, height: Int) {
        // Create a Bitmap from the RGBA bytes
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        pixels.rewind()
        bitmap.copyPixelsFromBuffer(pixels)

        // Save to a temporary URI via MediaStore so the clipboard can reference it
        val resolver = activity.contentResolver
        val values = android.content.ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, "gumicord_clipboard.png")
            put(MediaStore.Images.Media.MIME_TYPE, "image/png")
            put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/Gumicord")
        }

        val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)!!
        val outputStream = resolver.openOutputStream(uri)!!.use { output ->
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, output)
        }

        // Put the URI on the clipboard
        val clip = ClipData.newUri(activity.contentResolver, "gumicord_image", uri)
        clipboardManager.primaryClip = clip
    }

    /**
     * Gets an image from the clipboard.
     */
    fun getImage(): ImageData? {
        val clip = clipboardManager.primaryClip
        if (clip == null || clip.itemCount == 0) return null

        val item = clip.getItemAt(0)
        val uri = item.uri ?: return null

        // Decode the image from the URI
        val resolver = activity.contentResolver
        return try {
            val source = ImageDecoder.createSource(resolver, uri)
            val bitmap = ImageDecoder.decodeBitmap(source)
            val baos = ByteArrayOutputStream()
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, baos)
            val pngBytes = baos.toByteArray()

            // Decode PNG to RGBA
            val decoded = BitmapFactory.decodeByteArray(pngBytes, 0, pngBytes.size) ?: return null
            val rgba = ByteArray(bitmap.width * bitmap.height * 4)
            bitmap.copyPixelsToBuffer(rgba.asByteBuffer())

            ImageData(bitmap.width, bitmap.height, rgba)
        } catch (e: Exception) {
            null
        }
    }

    data class ImageData(
        val width: Int,
        val height: Int,
        val rgba: ByteArray
    )
}
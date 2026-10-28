package dev.gumicord.app

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.core.content.FileProvider
import java.io.File
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
        clipboardManager.setPrimaryClip(clip)
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
     *
     * The PNG goes to the app's cache and the clipboard holds a FileProvider
     * URI: MediaStore would need WRITE_EXTERNAL_STORAGE below API 29 and
     * would leave every copied image in the user's gallery. The system
     * clipboard grants read access to whichever app pastes.
     */
    fun setImage(pixels: ByteBuffer, width: Int, height: Int) {
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        pixels.rewind()
        bitmap.copyPixelsFromBuffer(pixels)

        val dir = File(activity.cacheDir, "clipboard")
        if (!dir.isDirectory && !dir.mkdirs()) {
            throw IllegalStateException("cannot create the clipboard directory")
        }
        val file = File(dir, "clipboard.png")
        file.outputStream().use { output ->
            if (!bitmap.compress(Bitmap.CompressFormat.PNG, 100, output)) {
                throw IllegalStateException("cannot write the clipboard image")
            }
        }

        val uri = FileProvider.getUriForFile(
            activity,
            "${activity.packageName}.fileprovider",
            file,
        )
        val clip = ClipData.newUri(activity.contentResolver, "gumicord_image", uri)
        clipboardManager.setPrimaryClip(clip)
    }

    /**
     * Gets an image from the clipboard.
     *
     * `BitmapFactory` rather than `ImageDecoder`: the latter is API 28+ and
     * this app runs on 24, and it hands back hardware bitmaps that
     * `copyPixelsToBuffer` refuses. The buffer Rust reads is always RGBA.
     */
    fun getImage(): ImageData? {
        val clip = clipboardManager.primaryClip
        if (clip == null || clip.itemCount == 0) return null

        val item = clip.getItemAt(0)
        val uri = item.uri ?: return null

        val resolver = activity.contentResolver
        return try {
            val bitmap = resolver.openInputStream(uri)?.use { input ->
                BitmapFactory.decodeStream(input)
            } ?: return null
            val argb = if (bitmap.config == Bitmap.Config.ARGB_8888) {
                bitmap
            } else {
                bitmap.copy(Bitmap.Config.ARGB_8888, false) ?: return null
            }
            val rgba = ByteArray(argb.width * argb.height * 4)
            argb.copyPixelsToBuffer(ByteBuffer.wrap(rgba))

            ImageData(argb.width, argb.height, rgba)
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
package dev.gumicord.app

import android.app.Activity
import android.app.Activity.RESULT_OK
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.content.ContentResolver
import android.content.ContentUris
import android.database.Cursor
import android.webkit.MimeTypeMap
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts

/**
 * File picker helper for Android using Storage Access Framework (SAF).
 * Called from Rust via JNI to implement file/folder pick and save operations.
 */
class FilePickerHelper(private val activity: Activity) {

    private var pendingCallback: ((Int, String?, Array<String>?) -> Unit)? = null

    // Request codes for different picker types
    private val FILE_PICK_CODE = 1001
    private val FILES_PICK_CODE = 1002
    private val FOLDER_PICK_CODE = 1003
    private val SAVE_FILE_CODE = 1004

    // Modern ActivityResult API launchers (API 30+)
    private var pickFileLauncher: ActivityResultLauncher<Intent>? = null
    private var pickFilesLauncher: ActivityResultLauncher<Intent>? = null
    private var pickFolderLauncher: ActivityResultLauncher<Intent>? = null
    private var saveFileLauncher: ActivityResultLauncher<Intent>? = null

    init {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            registerModernLaunchers()
        }
    }

    private fun registerModernLaunchers() {
        // For API 30+, we can use ActivityResultContracts
        // But we still need onActivityResult for compatibility
        // We'll use the traditional onActivityResult approach for simplicity
    }

    /**
     * Picks a single file.
     */
    fun pickFile(
        mimeTypes: Array<String>,
        extensions: Array<String>,
        title: String,
        startDir: String,
        multi: Boolean,
        directory: Boolean,
        save: Boolean,
        callback: (Int, String?, Array<String>?) -> Unit
    ) {
        pendingCallback = callback

        val intent = if (save) {
            createSaveFileIntent(mimeTypes, extensions, title)
        } else if (directory) {
            createFolderPickIntent(title)
        } else {
            createFilePickIntent(mimeTypes, extensions, title, multi)
        }

        activity.startActivityForResult(intent, when {
            save -> SAVE_FILE_CODE
            directory -> FOLDER_PICK_CODE
            multi -> FILES_PICK_CODE
            else -> FILE_PICK_CODE
        })
    }

    private fun createFilePickIntent(mimeTypes: Array<String>, extensions: Array<String>, title: String, multi: Boolean): Intent {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = mimeTypes.joinToString("|")
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multi)
            putExtra(Intent.EXTRA_TITLE, title)
        }
        return intent
    }

    private fun createFilesPickIntent(mimeTypes: Array<String>, extensions: Array<String>, title: String): Intent {
        return createFilePickIntent(mimeTypes, extensions, title, true)
    }

    private fun createFolderPickIntent(title: String): Intent {
        return Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            putExtra(Intent.EXTRA_TITLE, title)
        }
    }

    private fun createSaveFileIntent(mimeTypes: Array<String>, extensions: Array<String>, title: String): Intent {
        // For save, we use ACTION_CREATE_DOCUMENT
        return Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = mimeTypes.firstOrNull() ?: "*/*"
            putExtra(Intent.EXTRA_TITLE, title)
        }
    }

    /**
     * Called from Activity.onActivityResult
     */
    fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        val callback = pendingCallback ?: return
        pendingCallback = null

        if (resultCode != RESULT_OK) {
            callback(4, null, null) // Cancelled
            return
        }

        data?.data?.let { uri ->
            when (requestCode) {
                FILE_PICK_CODE, SAVE_FILE_CODE -> {
                    val path = getPathFromUri(uri)
                    callback(if (save) 3 else 0, path, null)
                }
                FILES_PICK_CODE -> {
                    data.clipData?.let { clipData ->
                        val paths = mutableListOf<String>()
                        for (i in 0 until clipData.itemCount) {
                            clipData.getItemAt(i).uri?.let { u ->
                                getPathFromUri(u)?.let { paths.add(it) }
                            }
                        }
                        callback(1, null, paths.toTypedArray())
                    } ?: run {
                        // Single file returned via data.data
                        getPathFromUri(uri)?.let { callback(1, null, arrayOf(it)) } ?: callback(4, null, null)
                    }
                }
                FOLDER_PICK_CODE -> {
                    // For folder, we return the tree URI
                    val path = getPathFromUri(uri)
                    callback(2, path, null)
                }
            }
        } ?: run {
            callback(5, "no uri", null) // Failed
        }
    }

    private fun getPathFromUri(uri: Uri): String? {
        return try {
            when {
                DocumentsContract.isDocumentUri(activity, uri) -> {
                    val docId = DocumentsContract.getDocumentId(uri)
                    val authority = uri.authority

                    if (authority == "com.android.externalstorage.documents") {
                        // ExternalStorageProvider
                        val parts = docId.split(":")
                        if (parts.size == 2) {
                            val type = parts[0]
                            val path = parts[1]
                            if (type == "primary") {
                                "/storage/emulated/0/$path"
                            } else {
                                "/storage/$type/$path"
                            }
                        } else {
                            uri.toString()
                        }
                    } else if (authority == "com.android.providers.downloads.documents") {
                        // DownloadsProvider
                        val id = docId.split(":").lastOrNull()?.toLongOrNull()
                        id?.let { contentUri ->
                            ContentUris.withAppendedId(Uri.parse("content://downloads/public_downloads"), it)
                        }?.toString() ?: uri.toString()
                    } else if (authority == "com.android.providers.media.documents") {
                        // MediaProvider
                        val parts = docId.split(":")
                        if (parts.size == 2) {
                            val type = parts[0]
                            val id = parts[1]
                            val contentUri = when (type) {
                                "image" -> MediaStore.Images.Media.EXTERNAL_CONTENT_URI
                                "video" -> MediaStore.Video.Media.EXTERNAL_CONTENT_URI
                                "audio" -> MediaStore.Audio.Media.EXTERNAL_CONTENT_URI
                                else -> MediaStore.Files.getContentUri("external")
                            }
                            val selection = "${MediaStore.Files.FileColumns._ID}=?"
                            val selectionArgs = arrayOf(id)
                            activity.contentResolver.query(contentUri, null, selection, selectionArgs, null)?.use { cursor ->
                                if (cursor.moveToFirst()) {
                                    val columnIndex = cursor.getColumnIndex(MediaStore.MediaColumns.DATA)
                                    if (columnIndex >= 0) {
                                        cursor.getString(columnIndex)
                                    } else {
                                        null
                                    }
                                }
                            } ?: uri.toString()
                        } else {
                            uri.toString()
                        }
                    } else {
                        uri.toString()
                    }
                }
                "content" == uri.scheme -> {
                    // Content URI - try to get real path
                    getPathFromContentUri(uri)
                }
                "file" == uri.scheme -> {
                    uri.path
                }
                else -> {
                    uri.toString()
                }
            } catch (e: Exception) {
                uri.toString()
            }
        }

    private fun getPathFromContentUri(uri: Uri): String? {
        val projection = arrayOf(MediaStore.MediaColumns.DATA)
        activity.contentResolver.query(uri, projection, null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val columnIndex = cursor.getColumnIndexOrThrow(MediaStore.MediaColumns.DATA)
                return cursor.getString(columnIndex)
            }
        }
        return uri.toString()
    }
}
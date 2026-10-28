package dev.gumicord.app

import android.app.Activity
import android.content.ContentUris
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import java.io.File

/**
 * Storage Access Framework pickers, driven from Rust.
 *
 * GameActivity extends AppCompatActivity, so the activity-result API exists
 * here. The older `startActivityForResult` does not: GameActivity does not
 * override `onActivityResult`, so a picker launched that way would never
 * answer and the Rust caller would wait forever.
 *
 * The answer goes back through [FilePickerBridge.onResult], the native
 * function exported by `render/platform/src/file_dialog.rs`. Rust waits on a
 * condition variable while the picker is open; that call is what wakes it.
 *
 * Picked *files* are copied into the app cache and that copy's path is what
 * Rust receives. A SAF document URI cannot be opened with a plain filesystem
 * read under scoped storage, and the file may live in a cloud provider that
 * has no path at all. Folders keep the SAF path/URI: copying a whole tree is
 * not a pick.
 *
 * The argument order of [pickFile] and its aliases is the JNI contract with
 * that file. Keep the two in step. `extensions` and `startDir` have no SAF
 * equivalent (SAF filters by MIME type and opens its own last location).
 *
 * There is no save entry point: the destination would be a SAF document, and
 * the Rust side writes with `std::fs`, which cannot reach one. Saving needs a
 * byte-oriented bridge, so the Rust side answers `Unsupported` instead.
 */
@Suppress("UNUSED_PARAMETER")
class FilePickerHelper(private val activity: Activity) {

    private var pending = FilePickerBridge.FILE

    private val launcher: ActivityResultLauncher<Intent>? = try {
        (activity as? ComponentActivity)?.registerForActivityResult(
            ActivityResultContracts.StartActivityForResult()
        ) { result -> deliver(result.resultCode, result.data) }
    } catch (e: IllegalStateException) {
        // Registration is only legal before the activity is started. If the
        // shell built this too late, picking stays off instead of throwing
        // out of the constructor and taking the whole startup with it.
        Log.w(TAG, "activity-result registration came too late", e)
        null
    }

    /** Picks one file, several files, or a folder. */
    fun pickFile(
        title: String,
        mimeTypes: Array<String>,
        extensions: Array<String>,
        startDir: String,
        fileName: String,
        multi: Boolean,
        directory: Boolean,
    ) = launch(title, mimeTypes, fileName, multi, directory)

    fun pickFiles(
        title: String,
        mimeTypes: Array<String>,
        extensions: Array<String>,
        startDir: String,
        fileName: String,
        multi: Boolean,
        directory: Boolean,
    ) = launch(title, mimeTypes, fileName, multi, directory)

    fun pickFolder(
        title: String,
        mimeTypes: Array<String>,
        extensions: Array<String>,
        startDir: String,
        fileName: String,
        multi: Boolean,
        directory: Boolean,
    ) = launch(title, mimeTypes, fileName, multi, directory)

    private fun launch(
        title: String,
        mimeTypes: Array<String>,
        fileName: String,
        multi: Boolean,
        directory: Boolean,
    ) {
        pending = when {
            directory -> FilePickerBridge.FOLDER
            multi -> FilePickerBridge.FILES
            else -> FilePickerBridge.FILE
        }
        val intent = when {
            directory -> createFolderPickIntent(title)
            else -> createFilePickIntent(title, mimeTypes, multi)
        }
        val target = launcher
        if (target == null) {
            // Answer anyway: the Rust side is waiting for one.
            FilePickerBridge.onResult(FilePickerBridge.FAILED, null, null)
            return
        }
        // The previous pick has been read by now; its cache copy can go.
        clearPicked()
        // `launch` starts an activity, which the UI thread must do.
        activity.runOnUiThread {
            try {
                target.launch(intent)
            } catch (e: Exception) {
                // A finished activity cannot start one. Answer anyway, or
                // the Rust side waits for the full timeout.
                Log.w(TAG, "cannot start the picker", e)
                FilePickerBridge.onResult(FilePickerBridge.FAILED, null, null)
            }
        }
    }

    private fun createFilePickIntent(
        title: String,
        mimeTypes: Array<String>,
        multi: Boolean,
    ): Intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
        addCategory(Intent.CATEGORY_OPENABLE)
        // One type goes in `type`; several only filter through the extra.
        type = if (mimeTypes.size == 1) mimeTypes[0] else "*/*"
        putExtra(Intent.EXTRA_MIME_TYPES, mimeTypes)
        putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multi)
        putExtra(Intent.EXTRA_TITLE, title)
    }

    private fun createFolderPickIntent(title: String): Intent =
        Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            putExtra(Intent.EXTRA_TITLE, title)
        }

    private fun deliver(resultCode: Int, data: Intent?) {
        val kind = pending
        pending = FilePickerBridge.FILE

        if (resultCode != Activity.RESULT_OK) {
            FilePickerBridge.onResult(FilePickerBridge.CANCELLED, null, null)
            return
        }
        val intent = data ?: run {
            FilePickerBridge.onResult(FilePickerBridge.FAILED, null, null)
            return
        }

        // Several files arrive as ClipData. Some providers put even a single
        // pick in ClipData, so the single-file path falls back to it.
        if (kind == FilePickerBridge.FILES) {
            val uris = ArrayList<Uri>()
            intent.clipData?.let { clip ->
                for (i in 0 until clip.itemCount) {
                    clip.getItemAt(i).uri?.let(uris::add)
                }
            }
            intent.data?.let(uris::add)
            val paths = uris.mapIndexedNotNull { index, uri -> cacheCopy(uri, index) }
            if (paths.isEmpty()) {
                FilePickerBridge.onResult(FilePickerBridge.FAILED, null, null)
            } else {
                FilePickerBridge.onResult(
                    FilePickerBridge.FILES,
                    null,
                    paths.toTypedArray(),
                )
            }
            return
        }

        val uri = intent.data
            ?: intent.clipData?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.uri
        if (uri == null) {
            FilePickerBridge.onResult(FilePickerBridge.FAILED, null, null)
            return
        }
        // A folder has no filesystem path under scoped storage, so its SAF
        // URI is what comes back. Copying a whole tree is not a pick anyway.
        val path = when (kind) {
            FilePickerBridge.FOLDER -> uri.toString()
            else -> {
                val copied = cacheCopy(uri, 0)
                if (copied == null) {
                    FilePickerBridge.onResult(FilePickerBridge.FAILED, null, null)
                    return
                }
                copied
            }
        }
        FilePickerBridge.onResult(kind, path, null)
    }

    /** Empties the past picks; their caller has read them by now. */
    private fun clearPicked() {
        File(activity.cacheDir, "picked").listFiles()?.forEach { it.delete() }
    }

    /**
     * Copies a picked document into the app cache and returns that path.
     *
     * A SAF URI is not a filesystem path: scoped storage refuses a plain
     * read, and a cloud provider has no local file at all. The display name
     * is only a hint, so it is sanitized before it names a file.
     */
    private fun cacheCopy(uri: Uri, index: Int): String? = try {
        val dir = File(activity.cacheDir, "picked")
        if (!dir.isDirectory && !dir.mkdirs()) {
            Log.w(TAG, "cannot create the pick cache")
            null
        } else {
            val hint = displayName(uri)?.let(::sanitize) ?: "picked.bin"
            // Duplicate display names in one multi-pick need a prefix.
            val named = File(dir, hint)
            val file = if (named.exists()) File(dir, "${index}_$hint") else named
            val input = activity.contentResolver.openInputStream(uri)
            if (input == null) {
                Log.w(TAG, "cannot open $uri")
                null
            } else {
                input.use { source -> file.outputStream().use { source.copyTo(it) } }
                file.absolutePath
            }
        }
    } catch (e: Exception) {
        Log.w(TAG, "cannot copy $uri", e)
        null
    }

    private fun displayName(uri: Uri): String? =
        activity.contentResolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { cursor ->
                if (cursor.moveToFirst()) cursor.getString(0) else null
            }

    /** Keeps the provider's name from choosing the cache file's path. */
    private fun sanitize(name: String): String {
        val cleaned = name.replace(Regex("[^A-Za-z0-9._-]"), "_").trim('.')
        return if (cleaned.isEmpty()) "picked.bin" else cleaned.take(64)
    }

    /**
     * The filesystem path behind a document URI, or the URI itself when the
     * provider keeps its files somewhere only it can reach (cloud storage,
     * for example). Callers treat the string as a path.
     */
    private fun pathOf(uri: Uri): String? = try {
        when {
            uri.scheme == "file" -> uri.path ?: uri.toString()
            DocumentsContract.isDocumentUri(activity, uri) -> documentPath(uri)
            else -> contentPath(uri) ?: uri.toString()
        }
    } catch (e: Exception) {
        Log.w(TAG, "cannot resolve $uri", e)
        uri.toString()
    }

    private fun documentPath(uri: Uri): String? {
        val authority = uri.authority ?: return uri.toString()
        // A tree URI (folder pick) has its own document-id accessor.
        val documentId = if (DocumentsContract.isTreeUri(uri)) {
            DocumentsContract.getTreeDocumentId(uri)
        } else {
            DocumentsContract.getDocumentId(uri)
        }
        return when (authority) {
            "com.android.externalstorage.documents" -> {
                val parts = documentId.split(":", limit = 2)
                if (parts.size != 2) return uri.toString()
                val volume = if (parts[0] == "primary") "emulated/0" else parts[0]
                "/storage/$volume/${parts[1]}"
            }
            "com.android.providers.downloads.documents" -> {
                val id = documentId.substringAfterLast(':').toLongOrNull()
                    ?: return uri.toString()
                val content = ContentUris.withAppendedId(
                    Uri.parse("content://downloads/public_downloads"),
                    id,
                )
                contentPath(content) ?: uri.toString()
            }
            "com.android.providers.media.documents" -> {
                val type = documentId.substringBefore(':')
                val id = documentId.substringAfterLast(':')
                val collection = when (type) {
                    "image" -> MediaStore.Images.Media.EXTERNAL_CONTENT_URI
                    "video" -> MediaStore.Video.Media.EXTERNAL_CONTENT_URI
                    "audio" -> MediaStore.Audio.Media.EXTERNAL_CONTENT_URI
                    else -> MediaStore.Files.getContentUri("external")
                }
                val projection = arrayOf(MediaStore.MediaColumns.DATA)
                activity.contentResolver
                    .query(collection, projection, "_id=?", arrayOf(id), null)
                    ?.use { cursor ->
                        if (cursor.moveToFirst()) cursor.getString(0) else null
                    } ?: uri.toString()
            }
            else -> uri.toString()
        }
    }

    private fun contentPath(uri: Uri): String? =
        activity.contentResolver
            .query(uri, arrayOf(MediaStore.MediaColumns.DATA), null, null, null)
            ?.use { cursor -> if (cursor.moveToFirst()) cursor.getString(0) else null }

    private companion object {
        const val TAG = "GumicordPicker"
    }
}

/**
 * The native half of the answer. A Kotlin `object` keeps the JNI symbol at
 * `Java_dev_gumicord_app_FilePickerBridge_onResult`; a companion `external`
 * would land on the companion class instead.
 */
object FilePickerBridge {
    const val FILE = 0
    const val FILES = 1
    const val FOLDER = 2
    const val SAVE = 3
    const val CANCELLED = 4
    const val FAILED = 5

    init {
        // GameActivity loads the same library first; this is a no-op then,
        // and keeps the bridge usable if that ever changes.
        System.loadLibrary("main")
    }

    external fun onResult(kind: Int, path: String?, paths: Array<String>?)
}

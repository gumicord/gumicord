package dev.gumicord.app

import android.app.Activity
import android.content.Context
import android.graphics.Color
import android.os.Build
import android.text.Editable
import android.text.InputType
import android.text.TextWatcher
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.FrameLayout

// The login autofill twins (ADR-0017). GameActivity's SurfaceView cannot be
// autofilled, so two EditText siblings sit over the rendered login fields:
// the credential provider pairs them by their autofill hints, and the
// focused one holds the keyboard while a login field is active. Rust decides
// when that happens and what the text means; this file only talks to the
// framework and hands answers back as plain values.
class LoginEditor(private val activity: Activity) {

    private val content: FrameLayout = activity.findViewById(android.R.id.content)
    private val imm: InputMethodManager =
        activity.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
    private val fields: List<EditText>
    // Written on the UI thread, read from Rust's event-loop thread.
    private val texts = arrayOf("", "")
    private var focusedIndex = UNSET
    private val actions = ArrayDeque<Int>()

    init {
        fields = listOf(
            makeField(
                USERNAME,
                InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS
                    or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS,
                View.AUTOFILL_HINT_USERNAME,
                EditorInfo.IME_ACTION_NEXT,
            ),
            makeField(
                PASSWORD,
                InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD,
                View.AUTOFILL_HINT_PASSWORD,
                EditorInfo.IME_ACTION_DONE,
            ),
        )
    }

    private fun makeField(index: Int, inputType: Int, hint: String, action: Int): EditText {
        val field = EditText(activity)
        field.inputType = inputType
        field.setSingleLine(true)
        // Both are Android 8+; below that the twins still hold the keyboard
        // so typing works, they just have nothing to offer. Both hint
        // constants are compile-time Strings, so reading them is free.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            field.setAutofillHints(hint)
            field.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_YES)
        }
        // TalkBack has its own pair of fields to read; these hold no text
        // of their own, and the rendered field draws what is typed.
        field.setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_NO)
        // Invisible but present: hidden or alpha-zero fields are skipped by
        // password autofill, and the app draws the text and the caret.
        field.setTextColor(Color.TRANSPARENT)
        field.setHintTextColor(Color.TRANSPARENT)
        field.cursorVisible = false
        field.background = null
        field.setPadding(0, 0, 0, 0)
        field.imeOptions = action or EditorInfo.IME_FLAG_NO_FULLSCREEN
        // Parked at a corner pixel until Rust places it, and untouchable
        // until Rust asks for it: a twin over the login screen that no
        // field is focused on would swallow the tap meant for the screen.
        field.layoutParams = FrameLayout.LayoutParams(1, 1)
        field.setFocusable(false)
        field.setFocusableInTouchMode(false)
        field.setClickable(false)
        field.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
            override fun afterTextChanged(s: Editable?) {
                synchronized(texts) { texts[index] = s?.toString() ?: "" }
            }
        })
        field.setOnEditorActionListener { _, actionId, _ ->
            if (actionId == EditorInfo.IME_ACTION_NEXT ||
                actionId == EditorInfo.IME_ACTION_DONE ||
                actionId == EditorInfo.IME_ACTION_GO ||
                actionId == EditorInfo.IME_ACTION_SEND
            ) {
                synchronized(actions) { actions.addLast(index) }
                true
            } else {
                false
            }
        }
        field.onFocusChangeListener = View.OnFocusChangeListener { _, hasFocus ->
            synchronized(texts) {
                if (hasFocus) {
                    focusedIndex = index
                } else if (focusedIndex == index) {
                    focusedIndex = UNSET
                }
            }
        }
        content.addView(field)
        return field
    }

    /** Moves one twin over its rendered field. Logical pixels, like the frame. */
    fun place(index: Int, x: Float, y: Float, w: Float, h: Float) {
        activity.runOnUiThread {
            val density = activity.resources.displayMetrics.density
            fields[index].layoutParams = FrameLayout.LayoutParams(
                (w * density).toInt().coerceAtLeast(1),
                (h * density).toInt().coerceAtLeast(1),
            ).apply {
                leftMargin = (x * density).toInt()
                topMargin = (y * density).toInt()
            }
        }
    }

    /** Gives one twin the keyboard, seeding it from the document once. */
    fun focus(index: Int, text: String) {
        activity.runOnUiThread {
            val field = fields[index]
            // Touchable only now: a twin that cannot be touched gets no
            // candidates, and it must not eat taps before this point.
            field.setFocusable(true)
            field.setFocusableInTouchMode(true)
            field.setClickable(true)
            if (field.text.isEmpty() && text.isNotEmpty()) {
                field.setText(text)
            }
            field.requestFocus()
            field.setSelection(field.text.length)
            imm.showSoftInput(field, InputMethodManager.SHOW_IMPLICIT)
        }
    }

    /** Drops the keyboard, forgets both fields, and goes untouchable. */
    fun blur() {
        activity.runOnUiThread {
            for (field in fields) {
                field.clearFocus()
                field.setText("")
                field.setFocusable(false)
                field.setFocusableInTouchMode(false)
                field.setClickable(false)
            }
            imm.hideSoftInputFromWindow(content.windowToken, 0)
        }
    }

    /** Puts a twin back at a corner pixel, out of every screen's way. */
    fun park(index: Int) {
        activity.runOnUiThread {
            fields[index].layoutParams = FrameLayout.LayoutParams(1, 1)
        }
    }

    /** One twin's current text, for the poll. */
    fun text(index: Int): String = synchronized(texts) { texts[index] }

    /** Which twin the keyboard sits on, if any; [UNSET] for none. */
    fun focused(): Int = synchronized(texts) { focusedIndex }

    /** The oldest action button, as its twin's index; [UNSET] for none. */
    fun pollAction(): Int = synchronized(actions) {
        if (actions.isEmpty()) UNSET else actions.removeFirst()
    }

    /** Writes a twin's text back, e.g. after a newline was scrubbed. */
    fun setText(index: Int, text: String) {
        // The cache first: Rust compares against it next tick, and the
        // watcher would only record what is already there.
        synchronized(texts) { texts[index] = text }
        activity.runOnUiThread { fields[index].setText(text) }
    }

    companion object {
        const val USERNAME = 0
        const val PASSWORD = 1

        /** Kotlin's "nothing", matching the Rust side. */
        const val UNSET = -1
    }
}

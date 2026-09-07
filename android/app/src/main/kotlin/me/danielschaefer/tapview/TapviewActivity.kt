package me.danielschaefer.tapview

import android.app.NativeActivity
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.view.WindowInsets
import android.view.WindowManager

/**
 * The app's one activity. NativeActivity loads libtapview_android.so (named
 * by the manifest's android.app.lib_name meta-data) and hands control to its
 * android_main; everything else — the UI, the HID protocol — lives on the
 * Rust side, which calls back into [UsbBridge] over JNI.
 */
class TapviewActivity : NativeActivity() {
    /**
     * "left top right bottom" system-bar overlap in pixels, maintained on the
     * UI thread (inset dispatch), read over JNI from the egui thread every
     * frame. A plain volatile string so the reader never touches View state
     * off the UI thread.
     */
    @Volatile
    var insetsStr: String = "0 0 0 0"

    /** Device name from a USB_DEVICE_ATTACHED launch, handed to Rust once. */
    @Volatile
    private var attachedPath: String? = null

    fun takeAttached(): String? = attachedPath.also { attachedPath = null }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.decorView.setOnApplyWindowInsetsListener { view, insets ->
            insetsStr = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                val i = insets.getInsets(
                    WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout()
                )
                "${i.left} ${i.top} ${i.right} ${i.bottom}"
            } else {
                @Suppress("DEPRECATION")
                "${insets.systemWindowInsetLeft} ${insets.systemWindowInsetTop} " +
                    "${insets.systemWindowInsetRight} ${insets.systemWindowInsetBottom}"
            }
            view.onApplyWindowInsets(insets)
        }
        // NativeActivity's own onCreate sets LAYOUT_IN_SCREEN|LAYOUT_INSET_DECOR,
        // which lays the window out under the status bar and reports the safe
        // area via insets — which winit 0.30 ignores, leaving the top of the
        // egui UI unreachable behind the clock. Clear them so the window is
        // laid out *between* the system bars instead (the Theme.Tapview v35
        // overlay opts out of Android 15+'s edge-to-edge enforcement for the
        // same reason). The bottom gesture bar still overlaps; the Rust side
        // pads it from the insets reported above.
        @Suppress("DEPRECATION")
        window.clearFlags(
            WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN or
                WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS or
                WindowManager.LayoutParams.FLAG_LAYOUT_INSET_DECOR
        )
        handleUsbIntent(intent)
    }

    // launchMode="singleTask": a plug-in while we are running arrives here.
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleUsbIntent(intent)
    }

    private fun handleUsbIntent(intent: Intent?) {
        if (intent?.action != UsbManager.ACTION_USB_DEVICE_ATTACHED) return
        val dev: UsbDevice? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
        }
        attachedPath = dev?.deviceName
    }
}

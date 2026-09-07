package me.danielschaefer.tapview

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbEndpoint
import android.hardware.usb.UsbInterface
import android.hardware.usb.UsbManager
import android.os.Build
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

/**
 * Everything USB, in one object the Rust side drives over JNI (see
 * `src/android_hid.rs` in the main crate, which constructs this at startup
 * and mirrors the method signatures and status codes below).
 *
 * Android has no /dev/hidraw*: the app gets a [UsbDeviceConnection] from
 * [UsbManager] once the user grants access, and does raw transfers on it.
 * [open] claims one HID interface with force=true, which detaches Android's
 * own hid-multitouch driver — from then on this app receives every report
 * (touch reports via [read] on the interrupt IN endpoint, feature reports via
 * [setFeature]/[getFeature] on EP0) and the pad stops moving the system
 * pointer. [close] releases the interface and the kernel rebinds its driver.
 *
 * Results cross JNI as plain tab-separated text rather than JSON so the Rust
 * side needs no extra dependency: see [list] and [descriptors].
 *
 * Permission is asynchronous by design: an open of an ungranted device posts
 * the system dialog and returns [PERMISSION_PENDING]; the Rust side shows
 * that and retries once [generation] changes. The device_filter.xml intent
 * filter makes the grant automatic when the user picks this app on plug-in.
 */
class UsbBridge(private val activity: Activity) {
    companion object {
        // Status codes returned by open() below zero; a non-negative value is
        // a live handle. Mirrored in android_hid.rs.
        const val GONE = -1
        const val PERMISSION_PENDING = -2
        const val NO_INTERFACE = -3
        const val CLAIM_FAILED = -4

        private const val ACTION_USB_PERMISSION = "me.danielschaefer.tapview.USB_PERMISSION"

        // Standard GET_DESCRIPTOR for the class-specific Report descriptor,
        // addressed to the interface: bmRequestType IN | standard | interface.
        private const val REQ_GET_DESCRIPTOR = 0x06
        private const val DESC_TYPE_REPORT = 0x22

        // HID class requests on EP0: bmRequestType OUT/IN | class | interface.
        private const val REQ_SET_REPORT = 0x09
        private const val REQ_GET_REPORT = 0x01
        private const val REPORT_TYPE_FEATURE = 0x0300

        private const val CONTROL_TIMEOUT_MS = 1000
        private const val MAX_DESCRIPTOR = 4096
    }

    private val manager = activity.getSystemService(Context.USB_SERVICE) as UsbManager

    private class Handle(
        val device: UsbDevice,
        val connection: UsbDeviceConnection,
        val iface: UsbInterface,
        val epIn: UsbEndpoint,
    )

    private val handles = ConcurrentHashMap<Int, Handle>()
    private var nextHandle = 0

    /** Bumped on every attach, detach and permission answer; Rust polls it. */
    private val generation = AtomicInteger(0)

    init {
        val filter = IntentFilter().apply {
            addAction(ACTION_USB_PERMISSION)
            addAction(UsbManager.ACTION_USB_DEVICE_ATTACHED)
            addAction(UsbManager.ACTION_USB_DEVICE_DETACHED)
        }
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.action == UsbManager.ACTION_USB_DEVICE_DETACHED) {
                    val gone: UsbDevice? =
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
                        } else {
                            @Suppress("DEPRECATION")
                            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
                        }
                    if (gone != null) closeDevice(gone.deviceName)
                }
                generation.incrementAndGet()
            }
        }
        // Our own permission action is setPackage'd, and system broadcasts are
        // delivered to not-exported receivers too.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            activity.registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            activity.registerReceiver(receiver, filter)
        }
    }

    private fun device(path: String): UsbDevice? =
        manager.deviceList.values.firstOrNull { it.deviceName == path }

    private fun hasHid(dev: UsbDevice): Boolean =
        (0 until dev.interfaceCount).any {
            dev.getInterface(it).interfaceClass == UsbConstants.USB_CLASS_HID
        }

    /**
     * Every attached USB device, one per line:
     * `path \t vid \t pid \t granted(0/1) \t hasHid(0/1) \t product`.
     * No transfers; safe to call often (Rust calls it once per [generation]).
     */
    fun list(): String {
        val sb = StringBuilder()
        for (dev in manager.deviceList.values) {
            val product = (dev.productName ?: "").replace('\t', ' ').replace('\n', ' ')
            sb.append(dev.deviceName).append('\t')
                .append(dev.vendorId).append('\t')
                .append(dev.productId).append('\t')
                .append(if (manager.hasPermission(dev)) 1 else 0).append('\t')
                .append(if (hasHid(dev)) 1 else 0).append('\t')
                .append(product).append('\n')
        }
        return sb.toString()
    }

    /**
     * The report descriptor of every HID interface of a granted device, one
     * per line: `interfaceIndex \t hex`. Null if the device is gone or not
     * granted. Each interface is claimed for the duration of the read:
     * usbfs refuses interface-addressed control transfers while the kernel's
     * usbhid owns the interface, so the descriptor is only readable once it
     * is ours (force=true detaches the driver; it rebinds on release).
     */
    fun descriptors(path: String): String? {
        val dev = device(path) ?: return null
        if (!manager.hasPermission(dev)) return null
        val conn = manager.openDevice(dev) ?: return null
        val sb = StringBuilder()
        try {
            for (i in 0 until dev.interfaceCount) {
                val iface = dev.getInterface(i)
                if (iface.interfaceClass != UsbConstants.USB_CLASS_HID) continue
                if (!conn.claimInterface(iface, true)) continue
                try {
                    val desc = ByteArray(MAX_DESCRIPTOR)
                    val n = conn.controlTransfer(
                        0x81, REQ_GET_DESCRIPTOR, DESC_TYPE_REPORT shl 8, iface.id,
                        desc, desc.size, CONTROL_TIMEOUT_MS,
                    )
                    if (n <= 0) continue
                    sb.append(i).append('\t')
                    for (b in 0 until n) sb.append(String.format("%02x", desc[b].toInt() and 0xFF))
                    sb.append('\n')
                } finally {
                    conn.releaseInterface(iface)
                }
            }
        } finally {
            conn.close()
        }
        return sb.toString()
    }

    /**
     * Post the system permission dialog for a device. The answer arrives as
     * a broadcast (bumping [generation]); nothing else receives it — the
     * Rust side simply re-lists and re-checks hasPermission.
     */
    fun requestPermission(path: String): Boolean {
        val dev = device(path) ?: return false
        if (manager.hasPermission(dev)) return true
        // The system fills extras into this intent, so it must be MUTABLE;
        // setPackage keeps a mutable broadcast legal on API 34+.
        val intent = Intent(ACTION_USB_PERMISSION).setPackage(activity.packageName)
        val pi = PendingIntent.getBroadcast(activity, 0, intent, PendingIntent.FLAG_MUTABLE)
        manager.requestPermission(dev, pi)
        return true
    }

    /**
     * Claim HID interface number [ifaceIndex] of the device at [path] (an
     * index into the device's interfaces, as reported by [descriptors]).
     * Returns a handle, or one of the negative status codes.
     */
    @Synchronized
    fun open(path: String, ifaceIndex: Int): Int {
        val dev = device(path) ?: return GONE
        if (!manager.hasPermission(dev)) {
            requestPermission(path)
            return PERMISSION_PENDING
        }
        if (ifaceIndex < 0 || ifaceIndex >= dev.interfaceCount) return NO_INTERFACE
        val iface = dev.getInterface(ifaceIndex)
        if (iface.interfaceClass != UsbConstants.USB_CLASS_HID) return NO_INTERFACE
        val conn = manager.openDevice(dev) ?: return GONE
        if (!conn.claimInterface(iface, true)) {
            conn.close()
            return CLAIM_FAILED
        }
        var epIn: UsbEndpoint? = null
        for (e in 0 until iface.endpointCount) {
            val ep = iface.getEndpoint(e)
            if (ep.type == UsbConstants.USB_ENDPOINT_XFER_INT && ep.direction == UsbConstants.USB_DIR_IN) {
                epIn = ep
            }
        }
        if (epIn == null) {
            conn.releaseInterface(iface)
            conn.close()
            return NO_INTERFACE
        }
        val handle = nextHandle++
        handles[handle] = Handle(dev, conn, iface, epIn)
        return handle
    }

    /** Release the interface (the kernel rebinds hid-multitouch) and close. */
    @Synchronized
    fun close(handle: Int) {
        handles.remove(handle)?.let {
            it.connection.releaseInterface(it.iface)
            it.connection.close()
        }
    }

    private fun closeDevice(path: String) {
        for ((handle, h) in handles) {
            if (h.device.deviceName == path) close(handle)
        }
    }

    /**
     * One interrupt IN transfer of up to [maxLen] bytes, waiting at most
     * [timeoutMs]. For numbered reports the first byte is the report ID, as
     * on the wire. Returns an empty array on timeout, null once the handle is
     * closed or the device has gone.
     */
    fun read(handle: Int, maxLen: Int, timeoutMs: Int): ByteArray? {
        val h = handles[handle] ?: return null
        val buf = ByteArray(maxOf(maxLen, h.epIn.maxPacketSize))
        val n = h.connection.bulkTransfer(h.epIn, buf, buf.size, timeoutMs)
        if (n < 0) {
            // Timeout and "device gone" both come back as -1: tell them apart
            // by whether the device is still enumerated.
            if (device(h.device.deviceName) == null) {
                close(handle)
                return null
            }
            return ByteArray(0)
        }
        return buf.copyOf(n)
    }

    /**
     * SET_REPORT(Feature) on EP0. [report] includes the report ID as its
     * first byte (hidraw's convention); for numbered reports the wire carries
     * that byte too, and the ID also goes in wValue's low byte — exactly what
     * the kernel's hidraw ioctl sends.
     */
    fun setFeature(handle: Int, report: ByteArray, timeoutMs: Int): Boolean {
        val h = handles[handle] ?: return false
        val reportId = report[0].toInt() and 0xFF
        val n = h.connection.controlTransfer(
            0x21, REQ_SET_REPORT, REPORT_TYPE_FEATURE or reportId, h.iface.id,
            report, report.size, timeoutMs,
        )
        return n >= 0
    }

    /**
     * GET_REPORT(Feature) on EP0. The device answers a numbered report with
     * the report ID as the first byte; whatever came back is returned as-is,
     * which is the shape hidraw's ioctl returns too.
     */
    fun getFeature(handle: Int, reportId: Int, length: Int, timeoutMs: Int): ByteArray? {
        val h = handles[handle] ?: return null
        val buf = ByteArray(length)
        val n = h.connection.controlTransfer(
            0xA1, REQ_GET_REPORT, REPORT_TYPE_FEATURE or reportId, h.iface.id,
            buf, length, timeoutMs,
        )
        if (n < 0) return null
        return buf.copyOf(n)
    }

    /** See [generation]. */
    fun generation(): Int = generation.get()

    /** The device the activity was launched for by USB_DEVICE_ATTACHED, once. */
    fun takeAttachedPath(): String? = (activity as? TapviewActivity)?.takeAttached()

    /** "left top right bottom" system-bar overlap in pixels; see TapviewActivity. */
    fun insets(): String = (activity as? TapviewActivity)?.insetsStr ?: "0 0 0 0"
}

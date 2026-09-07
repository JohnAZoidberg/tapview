use super::HidDevice;
use std::io;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

pub struct WinHidDevice {
    handle: HANDLE,
    /// Smallest buffer `HidD_GetFeature` accepts on this collection, or 0 if it
    /// could not be determined. See `get_feature`.
    min_get_feature_len: usize,
}

impl WinHidDevice {
    pub fn open(path: &Path) -> io::Result<Self> {
        let wide: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                0x80000000 | 0x40000000, // GENERIC_READ | GENERIC_WRITE
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

        Ok(Self {
            handle,
            min_get_feature_len: unsafe { min_get_feature_len(handle) },
        })
    }
}

/// Smallest buffer `HidD_GetFeature` accepts on this collection.
///
/// That is one byte more than `HIDP_CAPS::FeatureReportByteLength`, which is
/// itself the largest feature report plus its report ID byte. The extra byte is
/// what the Bluetooth HID transport reserves for its own header: over Bluetooth
/// a buffer of exactly `FeatureReportByteLength` fails with
/// ERROR_INSUFFICIENT_BUFFER. The USB and I2C transports accept either size, so
/// one padded buffer serves all three.
unsafe fn min_get_feature_len(handle: HANDLE) -> usize {
    let mut preparsed = PHIDP_PREPARSED_DATA::default();
    if !HidD_GetPreparsedData(handle, &mut preparsed) {
        return 0;
    }
    let mut caps = HIDP_CAPS::default();
    let len = if HidP_GetCaps(preparsed, &mut caps) == HIDP_STATUS_SUCCESS {
        caps.FeatureReportByteLength as usize + 1
    } else {
        0
    };
    let _ = HidD_FreePreparsedData(preparsed);
    len
}

impl WinHidDevice {
    fn get_feature_exact(&self, buf: &mut [u8]) -> io::Result<()> {
        let ok = unsafe {
            HidD_GetFeature(
                self.handle,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                buf.len() as u32,
            )
        };
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "HidD_GetFeature failed: {}",
                io::Error::last_os_error()
            )))
        }
    }
}

impl Drop for WinHidDevice {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl HidDevice for WinHidDevice {
    fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
        let ok = unsafe {
            HidD_SetFeature(
                self.handle,
                buf.as_ptr() as *const std::ffi::c_void,
                buf.len() as u32,
            )
        };
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "HidD_SetFeature failed: {}",
                io::Error::last_os_error()
            )))
        }
    }

    /// `HidD_GetFeature` sizes its buffer by the collection, not by the report
    /// being requested: anything shorter than `min_get_feature_len` is rejected
    /// outright, however short the report itself is. Over Bluetooth that
    /// applies to both the 4-byte register reports (0x42/0x43) and the burst
    /// report (0x41), so run every read through a large enough scratch buffer
    /// and copy back only what the caller asked for.
    fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.len() >= self.min_get_feature_len {
            return self.get_feature_exact(buf).map(|()| buf.len());
        }

        let mut scratch = vec![0u8; self.min_get_feature_len];
        scratch[0] = buf[0]; // report ID
        self.get_feature_exact(&mut scratch)?;
        buf.copy_from_slice(&scratch[..buf.len()]);
        Ok(buf.len())
    }
}

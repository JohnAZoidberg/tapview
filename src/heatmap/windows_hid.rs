use super::HidDevice;
use std::io;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

pub struct WinHidDevice {
    handle: HANDLE,
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

        Ok(Self { handle })
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
            Err(io::Error::other("HidD_SetFeature failed"))
        }
    }

    fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        let ok = unsafe {
            HidD_GetFeature(
                self.handle,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                buf.len() as u32,
            )
        };
        if ok {
            Ok(buf.len())
        } else {
            Err(io::Error::other("HidD_GetFeature failed"))
        }
    }
}

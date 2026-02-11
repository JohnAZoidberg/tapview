use super::HidDevice;
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

// HIDRAW ioctl numbers computed from the Linux _IOC macro:
//   _IOC(dir, type, nr, size)
//   type = 'H' = 0x48
//   HIDIOCSFEATURE = _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x06, len)
//   HIDIOCGFEATURE = _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x07, len)
//
// _IOC_WRITE = 1, _IOC_READ = 2 => dir = 3
// _IOC(dir, type, nr, size) = (dir << 30) | (size << 16) | (type << 8) | nr

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << 30) | (size << 16) | (ty << 8) | nr) as libc::c_ulong
}

fn hidiocsfeature(len: u32) -> libc::c_ulong {
    ioc(IOC_WRITE | IOC_READ, b'H' as u32, 0x06, len)
}

fn hidiocgfeature(len: u32) -> libc::c_ulong {
    ioc(IOC_WRITE | IOC_READ, b'H' as u32, 0x07, len)
}

pub struct HidrawDevice {
    fd: OwnedFd,
}

impl HidrawDevice {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self {
            fd: OwnedFd::from(file),
        })
    }
}

impl HidDevice for HidrawDevice {
    fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
        let ret = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                hidiocsfeature(buf.len() as u32),
                buf.as_ptr(),
            )
        };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        let ret = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                hidiocgfeature(buf.len() as u32),
                buf.as_mut_ptr(),
            )
        };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }
}

// The light-ccb driver's V4L2 controls, on its subdev node (group video).

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;

// (the standard exposure and gain controls are libcamera's; the driver's own are these)
pub const EXPOSURE_AUTO: u32 = 0x009a_0901; // menu: 0 auto, 1 manual
pub const AF_START: u32 = 0x009a_091c;
pub const MODULE: u32 = 0x0098_1d00; // preview module: 0 A1, 1 B4, 2 C5
pub const EXPOSURE_US: u32 = 0x0098_1d02; // manual exposure time, 1 us .. 15 s
pub const ISO: u32 = 0x0098_1d03; // manual ISO, 100 .. 3200
pub const AE_EXPOSURE_US: u32 = 0x0098_1d04; // the ASICs' metering (read-only)
pub const AE_ISO: u32 = 0x0098_1d05;
pub const FOCUS_X: u32 = 0x0098_1d06; // the AF window's top left in the module's 4160x3120
pub const FOCUS_Y: u32 = 0x0098_1d07;
pub const FLASH: u32 = 0x0098_1d08; // 0 off, 1 auto, 2 on
pub const ZOOM: u32 = 0x0098_1d09; // the zoom factor x 1000 (1000: 28 mm)
pub const MIRRORS: u32 = 0x0098_1d0a; // button: the mirrors for the zoom (once it settles)
pub const METERING: u32 = 0x0098_1d0b; // 0 centre-weighted, 1 touch
pub const PRIORITY: u32 = 0x0098_1d0c; // in auto: 0 none, 1 ISO, 2 shutter
pub const EV: u32 = 0x0098_1d0d; // EV compensation in thirds, -9 .. 9
pub const PREVIEW_BOOST: u32 = 0x0098_1d0e; // the preview's digital boost x 100 (read-only)
pub const TRIPOD: u32 = 0x0098_1d0f; // the camera is still: auto photos up to 100 ms
pub const STACKED: u32 = 0x0098_1d10; // the next photo will be stacked (read-only)
pub const AF_RESULT: u32 = 0x0098_1d11; // the last focus run: 0 running, 1 focused, 2 not
pub const UTC_OFFSET: u32 = 0x0098_1d12; // local time's offset from UTC (s): the photos' time

// the ioctl request type: libc's on Linux (musl's differs from glibc's); elsewhere only so a
// desktop can compile the app for its demo mode (dev.rs)
#[cfg(target_os = "linux")]
pub type Ioctl = libc::Ioctl;
#[cfg(not(target_os = "linux"))]
pub type Ioctl = libc::c_ulong;

#[repr(C)]
struct Control {
    id: u32,
    value: i32,
}

// struct light_ccb_capture: a photo while previewing (LIGHT_CCB_IOC_CAPTURE)
#[repr(C)]
#[derive(Default)]
pub struct Capture {
    pub mask: u32,
    pub uuid: [u8; 16],
    pub records: [u16; 3],
    pub record_bytes: u32,
    pub status: i32,
    pub burst: u8,
    pub flags: u8,
    pub fps: u16,
}

pub const CAPTURE_NO_PRECAPTURE: u8 = 1; // the preview's exposure as it is
pub const CAPTURE_NO_STACK: u8 = 2; // one frame per module

const IOC_CAPTURE: u64 = 3 << 30 | 40 << 16 | (b'L' as u64) << 8 | 5;
const IOC_TRANSFER: u64 = 1 << 30 | 4 << 16 | (b'L' as u64) << 8 | 6;

const VIDIOC_G_CTRL: u64 = 0xc008_561b;
const VIDIOC_S_CTRL: u64 = 0xc008_561c;

pub struct Ccb {
    file: File,
}

impl Ccb {
    pub fn open() -> Option<Ccb> {
        for entry in fs::read_dir("/sys/class/video4linux").ok()?.flatten() {
            let name = fs::read_to_string(entry.path().join("name")).unwrap_or_default();
            if name.starts_with("light-ccb") {
                let dev = format!("/dev/{}", entry.file_name().to_string_lossy());
                match OpenOptions::new().read(true).write(true).open(&dev) {
                    Ok(file) => return Some(Ccb { file }),
                    Err(e) => eprintln!("nebula: {dev}: {e}"),
                }
            }
        }
        None
    }

    pub fn get(&self, id: u32) -> Option<i32> {
        let mut c = Control { id, value: 0 };
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), VIDIOC_G_CTRL as _, &mut c) };
        (r == 0).then_some(c.value)
    }

    // the preview pauses while the modules in @mask expose; the ASICs then hold the records
    pub fn capture(&self, mask: u32, burst: u8, flags: u8) -> Result<Capture, String> {
        let mut c = Capture { mask, burst, flags, ..Default::default() };
        if let Ok(mut f) = File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut c.uuid);
        }
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), IOC_CAPTURE as _, &mut c) };
        if r != 0 {
            return Err(format!("capture: {}", std::io::Error::last_os_error()));
        }
        Ok(c)
    }

    // ASIC @asic (0-2) sends its next record over virtual channel 1
    pub fn transfer(&self, asic: u32) -> Result<(), String> {
        let a = asic;
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), IOC_TRANSFER as _, &a) };
        if r != 0 {
            return Err(format!("transfer: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn set(&self, id: u32, value: i32) -> bool {
        let mut c = Control { id, value };
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), VIDIOC_S_CTRL as _, &mut c) };
        if r != 0 {
            eprintln!(
                "nebula: control {id:#x} = {value}: {}",
                std::io::Error::last_os_error()
            );
        }
        r == 0
    }
}

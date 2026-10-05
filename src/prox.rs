// The lens-blocked sensors: five TXC PA224 IR proximity sensors around the camera modules,
// at 0x1e on the SoC's I2C (four behind blsp1 I2C6's TCA9545A mux, one on blsp1 I2C5),
// read through i2c-dev as stock's HAL reads them: one at a time (their IR emitters see each
// other), a reading of 100 or more = covered (stock's LensObstructionDetector). Stock gives
// each 150 ms; 60 here (its own calibration reads them 50 ms apart), each sensor's bit
// updated as it is read, so a cover shows in a third of a second.
// Set up as stock's driver does (pa224_init_client, pa224_fast_run_calibration): the
// crosstalk is measured each time the preview starts. Only while `on` (the preview running).
// The buses are the video group's (device-light-lfc's udev rule).

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const ADDR: libc::c_ulong = 0x1e;
const BLOCKED: u8 = 100;
const SAMPLE: Duration = Duration::from_millis(60);

const I2C_SLAVE: crate::ccb::Ioctl = 0x0703;
const I2C_SMBUS: crate::ccb::Ioctl = 0x0720;
const SMBUS_BYTE_DATA: u32 = 2;

#[repr(C)]
struct SmbusData {
    block: [u8; 34],
}

#[repr(C)]
struct SmbusIoctl {
    read_write: u8,
    command: u8,
    size: u32,
    data: *mut SmbusData,
}

struct Sensor {
    file: File,
}

impl Sensor {
    fn open(bus: u32) -> Option<Sensor> {
        let file = OpenOptions::new().read(true).write(true).open(format!("/dev/i2c-{bus}")).ok()?;
        (unsafe { libc::ioctl(file.as_raw_fd(), I2C_SLAVE, ADDR) } == 0).then_some(Sensor { file })
    }

    fn xfer(&self, write: bool, reg: u8, val: u8) -> Option<u8> {
        let mut data = SmbusData { block: [0; 34] };
        data.block[0] = val;
        let mut args = SmbusIoctl { read_write: (!write) as u8, command: reg, size: SMBUS_BYTE_DATA, data: &mut data };
        (unsafe { libc::ioctl(self.file.as_raw_fd(), I2C_SMBUS, &mut args) } == 0).then_some(data.block[0])
    }

    fn read(&self, reg: u8) -> Option<u8> {
        self.xfer(false, reg, 0)
    }

    fn write(&self, reg: u8, val: u8) -> Option<()> {
        self.xfer(true, reg, val).map(|_| ())
    }

    fn ps_on(&self, on: bool) -> Option<()> {
        self.write(0x00, if on { 0x02 } else { 0x00 })
    }

    // stock's init, then its crosstalk measurement: 4 readings 50 ms apart with no offset,
    // the middle two's mean + 4 (none over 99: a lens covered right now)
    fn setup(&self) -> Option<()> {
        if self.read(0x7f)? != 0x11 {
            return None;
        }
        for (r, v) in [(0x01, 0x48), (0x03, 0x08), (0x11, 0x82), (0x12, 0x0c), (0x10, 0x00), (0x02, 0x00)] {
            self.write(r, v)?;
        }
        for (r, v) in [(0x0a, 0xff), (0x08, 0x00)] {
            self.write(r, v)?;
        }
        self.read(0x02)?;
        self.write(0x02, 0x08)?;
        self.ps_on(true)?;
        let mut s = [0u8; 4];
        for v in s.iter_mut() {
            thread::sleep(Duration::from_millis(50));
            *v = self.read(0x0e)?;
        }
        s.sort();
        let xt = (s[1] as u32 + s[2] as u32) / 2 + 4;
        self.write(0x10, if xt > 99 { 0 } else { xt as u8 })?;
        for (r, v) in [(0x0a, 40), (0x08, 25)] {
            self.write(r, v)?;
        }
        self.ps_on(false)
    }

    // one reading: the emitter on for SAMPLE
    fn sample(&self) -> Option<u8> {
        self.ps_on(true)?;
        thread::sleep(SAMPLE);
        let v = self.read(0x0e);
        self.ps_on(false)?;
        v
    }
}

// an I2C bus's number by its adapter: an of_node path ending, or a sysfs name
fn bus(matches: impl Fn(&Path) -> bool) -> Option<u32> {
    for e in fs::read_dir("/sys/bus/i2c/devices").ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(n) = name.strip_prefix("i2c-").and_then(|n| n.parse().ok()) {
            if matches(&e.path()) {
                return Some(n);
            }
        }
    }
    None
}

// stock's ch0..ch4: mux channels 1, 3, (I2C5), 0, 2. On stock's diagram, seen from the
// screen: ch0-2 down the left edge, ch3 top centre, ch4 bottom centre (checked by covering
// each in turn)
fn buses() -> Option<[u32; 5]> {
    let of = |p: &Path, end: &str| {
        fs::read_link(p.join("of_node")).is_ok_and(|l| l.to_string_lossy().ends_with(end))
    };
    let parent = bus(|p| of(p, "i2c@757a000"))?;
    let chan = |c: u32| {
        bus(|p| fs::read_to_string(p.join("name")).is_ok_and(|n| n.trim() == format!("i2c-{parent}-mux (chan_id {c})")))
    };
    Some([chan(1)?, chan(3)?, bus(|p| of(p, "i2c@7579000"))?, chan(0)?, chan(2)?])
}

// the thread: `blocked` has a bit per covered sensor (ch0 = bit 0) while `on`; 0 while off
pub fn spawn(on: Arc<AtomicBool>, blocked: Arc<AtomicU8>) {
    thread::spawn(move || {
        let Some(buses) = buses() else { return };
        let sensors: Vec<Option<Sensor>> = buses.iter().map(|&b| Sensor::open(b)).collect();
        if sensors.iter().all(Option::is_none) {
            eprintln!("nebula: proximity sensors: can't open their I2C buses");
            return;
        }
        loop {
            while !on.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(200));
            }
            let ok: Vec<bool> = sensors.iter().map(|s| s.as_ref().is_some_and(|s| s.setup().is_some())).collect();
            while on.load(Ordering::Relaxed) {
                for (i, s) in sensors.iter().enumerate() {
                    if !ok[i] {
                        continue;
                    }
                    let covered = s.as_ref().and_then(Sensor::sample).is_some_and(|v| v >= BLOCKED);
                    let bit = 1u8 << i;
                    if covered {
                        blocked.fetch_or(bit, Ordering::Relaxed);
                    } else {
                        blocked.fetch_and(!bit, Ordering::Relaxed);
                    }
                }
            }
            blocked.store(0, Ordering::Relaxed);
        }
    });
}

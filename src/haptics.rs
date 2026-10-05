// The L16's vibration motor (DW7800, a force-feedback input device), for stock's haptics:
// the exposure dials' start, ticks and end, and the zoom's dots and prime stops. Stock played
// Immersion effects; these are short rumbles of about the same feel.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;

const EV_FF: u16 = 0x15;
const FF_RUMBLE: u16 = 0x50;
const EVIOCSFF: crate::ccb::Ioctl = 0x4030_4580u32 as crate::ccb::Ioctl; // _IOW('E', 0x80, struct ff_effect): 48 bytes

pub struct Haptics {
    dev: Option<File>,
    // uploaded effects by (length ms, strength): their ids
    effects: RefCell<HashMap<(u16, u16), i16>>,
}

impl Haptics {
    pub fn open() -> Self {
        let dev = std::fs::read_dir("/sys/class/input").ok().and_then(|dir| {
            dir.flatten().find_map(|e| {
                let name = std::fs::read_to_string(e.path().join("device/name")).ok()?;
                let node = e.file_name().into_string().ok()?;
                (name.trim() == "dw7800-haptics" && node.starts_with("event")).then_some(node)
            })
        });
        let dev = dev.and_then(|node| {
            OpenOptions::new().read(true).write(true).open(format!("/dev/input/{node}")).ok()
        });
        if dev.is_none() {
            eprintln!("nebula: no vibration motor");
        }
        Haptics { dev, effects: RefCell::new(HashMap::new()) }
    }

    // a rumble of @ms at @strength (0-65535; 0: nothing)
    pub fn play(&self, ms: u16, strength: u16) {
        let Some(dev) = &self.dev else { return };
        if strength == 0 {
            return;
        }
        let key = (ms, strength);
        let known = self.effects.borrow().get(&key).copied();
        let id = match known {
            Some(id) => id,
            None => {
                // struct ff_effect: type, id (-1: new), direction, trigger {button, interval},
                // replay {length, delay}, pad, then the union: rumble {strong, weak}
                let mut e = [0u8; 48];
                e[0..2].copy_from_slice(&FF_RUMBLE.to_le_bytes());
                e[2..4].copy_from_slice(&(-1i16).to_le_bytes());
                e[10..12].copy_from_slice(&ms.to_le_bytes());
                e[16..18].copy_from_slice(&strength.to_le_bytes());
                e[18..20].copy_from_slice(&strength.to_le_bytes());
                if unsafe { libc::ioctl(dev.as_raw_fd(), EVIOCSFF, e.as_mut_ptr()) } < 0 {
                    return;
                }
                let id = i16::from_le_bytes([e[2], e[3]]);
                self.effects.borrow_mut().insert(key, id);
                id
            }
        };
        // struct input_event: timeval (2 x i64), type, code, value
        let mut ev = [0u8; 24];
        ev[16..18].copy_from_slice(&EV_FF.to_le_bytes());
        ev[18..20].copy_from_slice(&(id as u16).to_le_bytes());
        ev[20..24].copy_from_slice(&1i32.to_le_bytes());
        let _ = (&*dev).write_all(&ev);
    }
}

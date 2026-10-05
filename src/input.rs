// The shutter button and the touch strip, read from evdev (udev gives the logged-in user
// access to both). Events go to the UI over a channel.

use std::fs::{self, File};
use std::io::Read;
use std::sync::mpsc::Sender;
use std::thread;

pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_CAMERA: u16 = 212;
pub const KEY_CAMERA_FOCUS: u16 = 528;
const BTN_TOUCH: u16 = 330;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;

pub enum Ev {
    Key(u16, bool),
    StripTouch(bool),
    StripX(i32),
}

fn find(name: &str) -> Option<String> {
    for entry in fs::read_dir("/sys/class/input").ok()?.flatten() {
        let node = entry.file_name().to_string_lossy().into_owned();
        if !node.starts_with("event") {
            continue;
        }
        let dev_name = fs::read_to_string(entry.path().join("device/name")).unwrap_or_default();
        if dev_name.trim() == name {
            return Some(format!("/dev/input/{node}"));
        }
    }
    None
}

pub fn spawn(tx: Sender<Ev>) {
    for (name, strip) in [("gpio-keys", false), ("Light L16 touch strip", true)] {
        let Some(path) = find(name) else {
            eprintln!("l16-camera2: no input device {name}");
            continue;
        };
        let tx = tx.clone();
        thread::spawn(move || {
            let mut f = match File::open(&path) {
                Ok(f) => f,
                Err(e) => return eprintln!("l16-camera2: {path}: {e}"),
            };
            // struct input_event on 64-bit: timeval (16), type, code, value
            let mut buf = [0u8; 24];
            while f.read_exact(&mut buf).is_ok() {
                let typ = u16::from_le_bytes([buf[16], buf[17]]);
                let code = u16::from_le_bytes([buf[18], buf[19]]);
                let value = i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
                let ev = match (typ, code) {
                    (EV_KEY, BTN_TOUCH) if strip => Ev::StripTouch(value != 0),
                    (EV_ABS, ABS_X) if strip => Ev::StripX(value),
                    (EV_KEY, c) if !strip && value != 2 => Ev::Key(c, value != 0),
                    _ => continue,
                };
                if tx.send(ev).is_err() {
                    return;
                }
            }
        });
    }
}

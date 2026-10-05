// White balance presets (stock's: auto, incandescent, fluorescent, daylight, cloudy) from this
// camera's own factory colour calibration: the LRI device blocks hold, for every module, the
// sensor's red/green and blue/green response under standard illuminants (A, D65, F11), and a
// preset's gains are their inverses. The blocks are this camera's (kept on the device only);
// without them, typical L16 values are used.

use std::collections::HashMap;

pub const PRESETS: [&str; 5] = ["auto", "incandescent", "fluorescent", "daylight", "cloudy"];
// the LRI view preferences' AWBMode for each (auto, tungsten, fluorescent, daylight, cloudy)
pub const AWB_MODE: [u8; 5] = [0, 4, 5, 1, 3];

// the camera's factory calibration: lightcal's calibration.lri (gathered at boot by
// light-lfc-android-libs), or blocks cut from one of its stock LRIs by hand (older installs),
// as l16-lri-assemble reads them
const DEVICE_BLOCKS: [&str; 2] = ["/var/lib/l16/calibration.lri", "/var/lib/l16/lri-device-blocks.bin"];
// ColorCalibration's IlluminantType
const ILLUM_A: u64 = 0;
const ILLUM_D65: u64 = 2;
const ILLUM_F11: u64 = 6;
// the LRI's camera IDs of the preview modules, A1 and B4
const PREVIEW_IDS: [u64; 2] = [0, 8];
// typical gains (red, blue) for the presets, when there is no calibration
const TYPICAL: [(f32, f32); 4] = [(1.25, 2.2), (1.65, 1.9), (1.8, 1.55), (1.95, 1.5)];

#[derive(Default)]
pub struct Calibration {
    // camera ID: (illuminant, r/g, b/g)
    modules: HashMap<u64, Vec<(u64, f32, f32)>>,
}

enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        v |= ((c & 0x7f) as u64).checked_shl(shift)?;
        shift += 7;
        if c < 0x80 {
            return Some(v);
        }
    }
}

// a protobuf message's fields, or None if it does not parse as one
fn fields(b: &[u8]) -> Option<Vec<(u64, Value<'_>)>> {
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        let key = varint(b, &mut i)?;
        if key >> 3 == 0 {
            return None;
        }
        let v = match key & 7 {
            0 => Value::Int(varint(b, &mut i)?),
            1 => {
                i += 8;
                Value::Bytes(b.get(i - 8..i)?)
            }
            2 => {
                let n = varint(b, &mut i)? as usize;
                i += n;
                Value::Bytes(b.get(i - n..i)?)
            }
            5 => {
                i += 4;
                Value::Bytes(b.get(i - 4..i)?)
            }
            _ => return None,
        };
        out.push((key >> 3, v));
    }
    Some(out)
}

fn f32_of(v: &Value) -> Option<f32> {
    match v {
        Value::Bytes(b) if b.len() == 4 => Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        _ => None,
    }
}

// a ColorCalibration: type (1), rg_ratio (4), bg_ratio (5)
fn color_calibration(b: &[u8]) -> Option<(u64, f32, f32)> {
    let fs = fields(b)?;
    let get = |n| fs.iter().find(|(k, _)| *k == n).map(|(_, v)| v);
    let Value::Int(t) = get(1)? else { return None };
    Some((*t, f32_of(get(4)?)?, f32_of(get(5)?)?))
}

impl Calibration {
    pub fn load() -> Self {
        let mut cal = Calibration::default();
        let Some(data) = DEVICE_BLOCKS.iter().find_map(|p| std::fs::read(p).ok()) else { return cal };
        // LELR blocks: {"LELR", u64 length, u64 message offset, u32 message length, ...}
        let mut o = 0;
        while o + 32 <= data.len() && &data[o..o + 4] == b"LELR" {
            let u64_at = |p: usize| u64::from_le_bytes(data[p..p + 8].try_into().unwrap());
            let (len, off) = (u64_at(o + 4) as usize, u64_at(o + 12) as usize);
            let n = u32::from_le_bytes(data[o + 20..o + 24].try_into().unwrap()) as usize;
            if len == 0 {
                break;
            }
            if let Some(msg) = data.get(o + off..o + off + n) {
                cal.walk(msg, 0);
            }
            o += len;
        }
        for (name, id) in [("A1", 0), ("B4", 8)] {
            eprintln!("nebula: colour calibration {name}: {:?}", cal.modules.get(&id));
        }
        cal
    }

    // ColorCalibrationGold: camera_id (1), ColorCalibration data (2, repeated), anywhere inside
    fn walk(&mut self, b: &[u8], depth: u32) {
        let Some(fs) = fields(b) else { return };
        if depth > 8 {
            return;
        }
        let id = fs.iter().find_map(|(k, v)| match (k, v) {
            (1, Value::Int(i)) => Some(*i),
            _ => None,
        });
        let cals: Vec<_> = fs
            .iter()
            .filter_map(|(k, v)| match (k, v) {
                (2, Value::Bytes(d)) => color_calibration(d),
                _ => None,
            })
            .collect();
        if let (Some(id), false) = (id, cals.is_empty()) {
            // one entry per illuminant, each naming the module again
            self.modules.entry(id).or_default().extend(cals);
            return;
        }
        for (_, v) in &fs {
            if let Value::Bytes(d) = v {
                if d.len() > 8 {
                    self.walk(d, depth + 1);
                }
            }
        }
    }

    // a preset's gains (red, blue) for preview module @module (0 A1, 1 B4); None for auto
    pub fn gains(&self, preset: usize, module: usize) -> Option<(f32, f32)> {
        if preset == 0 {
            return None;
        }
        let ratio = |illum| {
            let m = self.modules.get(&PREVIEW_IDS[module.min(1)])?;
            m.iter().find(|c| c.0 == illum).map(|c| (c.1, c.2))
        };
        let rg_bg = match preset {
            1 => ratio(ILLUM_A),
            2 => ratio(ILLUM_F11),
            // daylight, 5500 K: between D65 and A, linear in reciprocal colour temperature
            3 => ratio(ILLUM_D65).zip(ratio(ILLUM_A)).map(|(d, a)| {
                let t = (1.0 / 5500.0 - 1.0 / 6504.0) / (1.0 / 2856.0 - 1.0 / 6504.0);
                (d.0 + t * (a.0 - d.0), d.1 + t * (a.1 - d.1))
            }),
            _ => ratio(ILLUM_D65),
        };
        Some(match rg_bg {
            Some((rg, bg)) if rg > 0.0 && bg > 0.0 => (1.0 / rg, 1.0 / bg),
            _ => TYPICAL[preset - 1],
        })
    }
}

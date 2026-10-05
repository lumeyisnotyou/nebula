// The sparkle: a burst of light from the status LED by the shutter button when a photo is taken,
// in the accent colour. The LED is the PMIC's multicolour LPG, which the charging-light service
// (light-lfc-led) also drives: its breathing pattern is a hardware pattern, so this saves what
// the LED was doing (the pattern, or a plain brightness), plays the burst, and puts it back.
//
// The LPG plays a pattern whose steps are all the same length (the first and last pauses
// aside), so the burst is brightness nodes at an equal spacing. The attributes written are the
// feedbackd group's (device-light-lfc's udev rule), which the user is in; the trigger, which
// only root may change, is left alone.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const LED: &str = "/sys/class/leds/rgb:status";
const MAX: u32 = 511;
// a quick flicker of three flashes, dying away
const NODES: [u32; 12] = [0, MAX, 120, MAX, 200, MAX, MAX, 300, 140, 60, 20, 0];
const STEP_MS: u64 = 45;

static BUSY: AtomicBool = AtomicBool::new(false);

fn read(file: &str) -> Option<String> {
    fs::read_to_string(format!("{LED}/{file}")).ok().map(|s| s.trim().to_string())
}

fn write(file: &str, value: &str) -> bool {
    fs::write(format!("{LED}/{file}"), value).is_ok()
}

// the burst in @rgb (0..1 each), once at a time
pub fn sparkle(rgb: (f64, f64, f64)) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        run(rgb);
        BUSY.store(false, Ordering::SeqCst);
    });
}

fn run(rgb: (f64, f64, f64)) {
    // the channels' order (blue green red), each at its share of the colour
    let Some(index) = read("multi_index") else { return };
    let want: Vec<String> = index
        .split_whitespace()
        .map(|c| {
            let v = match c {
                "red" => rgb.0,
                "green" => rgb.1,
                _ => rgb.2,
            };
            ((v * 255.0).round() as u32).to_string()
        })
        .collect();
    let patterned = read("trigger").is_some_and(|t| t.contains("[pattern]"));
    let (intensity, brightness) = (read("multi_intensity"), read("brightness"));
    if !write("multi_intensity", &want.join(" ")) {
        return;
    }
    if patterned {
        let (pattern, repeat) = (read("hw_pattern"), read("repeat"));
        // each node: ramp to it over a step, hold it for none; the first pause is none
        let mut p = String::new();
        for (k, v) in NODES.iter().enumerate() {
            let t = if k == 0 { 0 } else { STEP_MS };
            p.push_str(&format!("{v} {t} {v} 0 "));
        }
        if write("repeat", "1") && write("hw_pattern", p.trim()) {
            thread::sleep(Duration::from_millis(STEP_MS * NODES.len() as u64 + 120));
        }
        // back to what it was: the repeat first, then the pattern, as the service sets them
        write("repeat", repeat.as_deref().unwrap_or("-1"));
        if let Some(pattern) = pattern {
            write("hw_pattern", &pattern);
        }
    } else {
        // no pattern: brightness by hand, 15 ms apart, between the nodes
        let per = (STEP_MS / 15).max(1) as u32;
        for pair in NODES.windows(2) {
            for k in 0..per {
                let v = pair[0] as f64 + (pair[1] as f64 - pair[0] as f64) * (k + 1) as f64 / per as f64;
                write("brightness", &(v.round() as u32).to_string());
                thread::sleep(Duration::from_millis(15));
            }
        }
        write("brightness", brightness.as_deref().unwrap_or("0"));
    }
    if let Some(i) = intensity {
        write("multi_intensity", &i);
    }
}

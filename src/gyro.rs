// Stillness, from the gyro (the SLPI's BMI160, qcom-smgr-gyro): for tripod mode, which lets
// an auto photo take up to 100 ms rather than the handheld 42 ms. Stock turns it on while the
// camera is still (its trigger is in a library we don't have). Here: how far the camera turns
// in each 100 ms (the longest such photo), against what blurs it by BLUR_PX pixels at the
// zoom in use (a pixel is 0.30 mrad at 28 mm, 0.055 mrad at 150 mm), so a steady hand can
// qualify at 28 mm while at 150 mm only a rest will. Still once it has stayed under that for
// SETTLE, moving again as soon as a window passes 1.5x it.
// It also tells AF-D when the camera has moved and settled again (`moved`): stock's
// SignificantMotionDetector, gyro flavour: a turn faster than 0.7 rad/s, then 300 ms
// without one.
// The gyro is read only while `on` is set (the preview running). Its buffer and device are
// the video group's (device-light-lfc's udev rule). L16_GYRO_DEBUG=1 prints the turns.

use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const BLUR_PX: f64 = 1.5;
const EXPOSURE: f64 = 0.1; // s: tripod mode's longest auto photo
const SETTLE: Duration = Duration::from_secs(1);
const MOTION: f64 = 0.7; // rad/s: stock's AF-D motion (gyro)
const MOTION_STABLE: Duration = Duration::from_millis(300);
// one pixel at 28 mm: the 13 MP modules' 1.1 um pixels over their 3.7 mm focal length
const PIXEL_28MM: f64 = 1.1e-3 / 3.7;

fn find() -> Option<(PathBuf, PathBuf)> {
    for e in fs::read_dir("/sys/bus/iio/devices").ok()?.flatten() {
        if fs::read_to_string(e.path().join("name")).ok()?.trim() == "qcom-smgr-gyro" {
            let dev = PathBuf::from("/dev").join(e.file_name());
            return Some((e.path(), dev));
        }
    }
    None
}

fn set(p: PathBuf, v: &str) -> std::io::Result<()> {
    fs::write(p, v)
}

// the x, y, z turn rates (s32 each) into the buffer, no timestamp; enabled or not
fn enable(sys: &PathBuf, on: bool) -> std::io::Result<()> {
    set(sys.join("buffer/enable"), "0")?;
    if on {
        for a in ["x", "y", "z"] {
            set(sys.join(format!("scan_elements/in_anglvel_{a}_en")), "1")?;
        }
        set(sys.join("scan_elements/in_timestamp_en"), "0")?;
        set(sys.join("buffer/length"), "64")?;
        set(sys.join("buffer/enable"), "1")?;
    }
    Ok(())
}

// the thread: `still` follows the camera while `on`; false while off. `focal`: the zoom's
// 35 mm focal length x 10
pub fn spawn(on: Arc<AtomicBool>, still: Arc<AtomicBool>, focal: Arc<AtomicU32>, moved: Arc<AtomicBool>) {
    thread::spawn(move || {
        let Some((sys, dev)) = find() else { return };
        let read = |f: &str, d: f64| fs::read_to_string(sys.join(f)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(d);
        let scale = read("in_anglvel_scale", 0.000015258);
        let rate = read("in_anglvel_sampling_frequency", 200.0);
        let n = ((rate * EXPOSURE).round() as usize).max(1);
        let debug = std::env::var_os("L16_GYRO_DEBUG").is_some();
        loop {
            while !on.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(200));
            }
            if let Err(e) = enable(&sys, true) {
                eprintln!("nebula: gyro: {e}");
                return;
            }
            let Ok(mut f) = fs::File::open(&dev) else { return };
            let mut quiet_since = Instant::now();
            let mut buf = [0u8; 12];
            // the last 100 ms of turn rates, and their sum
            let mut window = std::collections::VecDeque::with_capacity(n);
            let mut sum = [0f64; 3];
            let mut shown = Instant::now();
            let mut worst = 0f64;
            // AF-D: the last fast turn, until the camera has been steady after it
            let mut motion: Option<Instant> = None;
            while on.load(Ordering::Relaxed) {
                // the buffer can be found turned off with the device open (by the sensor
                // driver?), and a read then waits for ever: wait a second at most, and
                // turn it on again
                let mut pfd = libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                if unsafe { libc::poll(&mut pfd, 1, 1000) } <= 0 {
                    let off = fs::read_to_string(sys.join("buffer/enable")).map_or(true, |s| s.trim() != "1");
                    if off {
                        eprintln!("nebula: gyro: buffer found off, turning it on again");
                        let _ = set(sys.join("buffer/enable"), "1");
                    }
                    continue;
                }
                if f.read_exact(&mut buf).is_err() {
                    break;
                }
                let w: [f64; 3] =
                    std::array::from_fn(|k| i32::from_le_bytes(buf[k * 4..k * 4 + 4].try_into().unwrap()) as f64 * scale);
                // (the turn speed, not `rate`: that's the samples per second, for `turn` below)
                let speed = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt();
                if speed > MOTION {
                    motion = Some(Instant::now());
                } else if motion.is_some_and(|t| t.elapsed() >= MOTION_STABLE) {
                    motion = None;
                    moved.store(true, Ordering::Relaxed);
                }
                window.push_back(w);
                for k in 0..3 {
                    sum[k] += w[k];
                }
                if window.len() > n {
                    let old = window.pop_front().unwrap();
                    for k in 0..3 {
                        sum[k] -= old[k];
                    }
                }
                if window.len() < n {
                    continue;
                }
                // the turn in the last 100 ms, and what blurs a photo that long
                let turn = (sum[0] * sum[0] + sum[1] * sum[1] + sum[2] * sum[2]).sqrt() / rate;
                let mm = focal.load(Ordering::Relaxed).max(280) as f64 / 10.0;
                let limit = BLUR_PX * PIXEL_28MM * 28.0 / mm;
                if turn > limit {
                    quiet_since = Instant::now();
                }
                if turn > 1.5 * limit {
                    still.store(false, Ordering::Relaxed);
                } else if quiet_since.elapsed() >= SETTLE {
                    still.store(true, Ordering::Relaxed);
                }
                if debug {
                    worst = worst.max(turn);
                    if shown.elapsed() >= Duration::from_secs(1) {
                        eprintln!(
                            "nebula: gyro: worst turn {:.3} mrad in 100 ms (limit {:.3} at {mm:.0} mm), still {}",
                            worst * 1e3,
                            limit * 1e3,
                            still.load(Ordering::Relaxed)
                        );
                        (worst, shown) = (0.0, Instant::now());
                    }
                }
            }
            still.store(false, Ordering::Relaxed);
            let _ = enable(&sys, false);
        }
    });
}

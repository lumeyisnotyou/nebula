// Nebula: a camera app for the Light L16 on Linux, a pro-camera instrument display. It began as
// l16-camera, laid out after OpenLight (the L16's
// community camera app): exposure readout on the left, the preview, and on the right the
// shutter between the two exposure dials, the last photo above and the toolbar below.
//
// Preview: libcamera (libcamerasrc) on the light-ccb driver, which previews one module at a
// time (A1 28 mm, B4 70 mm, C5 150 mm); zoom in between is a crop. Exposure and focus go to
// the driver's controls directly; the ASICs meter and focus themselves. Photos: the
// preview stops and l16-capture takes an LRI with the modules for the zoom.

mod canvas;
mod ccb;
mod dev;
mod dots;
mod geo;
mod gyro;
mod haptics;
mod icons;
mod input;
mod led;
mod prox;
mod rotate;
mod ruler;
mod settings;
mod settings_ui;
mod transfer;
mod wb;
mod zoomview;

use gst::prelude::*;
use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use canvas::Canvas;
use dots::Roll;
use rotate::Rotator;
use ruler::Ruler;
use zoomview::ZoomView;

// OpenLight's value lists (res/values/arrays.xml)
const ISO: &[i32] = &[
    100, 125, 160, 200, 250, 320, 400, 500, 640, 800, 1000, 1250, 1600, 2400, 3200,
];
const SHUTTER: &[&str] = &[
    "1/8000", "1/6400", "1/5000", "1/4000", "1/3200", "1/2500", "1/2000", "1/1600", "1/1250",
    "1/1000", "1/800", "1/640", "1/500", "1/400", "1/320", "1/240", "1/200", "1/160", "1/150",
    "1/120", "1/100", "1/80", "1/60", "1/50", "1/40", "1/30", "1/24", "1/20", "1/15", "1/12",
    "1/10", "1/8", "1/6", "1/5", "1/4", "0.3", "0.4", "0.5", "0.6", "0.8", "1", "1.25", "1.66",
    "2", "2.5", "3.2", "4", "5", "6", "8", "10", "12", "15",
];
const TIMERS: &[u32] = &[0, 3, 5, 10, 20];
// OpenLight's burst modes (burst_3, burst_6)
const BURSTS: &[u8] = &[1, 3];
// zoom stops: OpenLight's primes, with the L16's real 70 mm B modules
const PRIMES: &[f64] = &[28.0, 35.0, 70.0, 150.0];
const ZOOM_MIN: f64 = 28.0;
const ZOOM_MAX: f64 = 150.0;
// the preview modules' focal lengths: A1, and B4 from 70 mm on (stock never previews on
// the 150 mm modules; it crops B4)
const MODULE_MM: [f64; 2] = [28.0, 70.0];
// orange: what the photographer has set; green: in focus, fine; red: clipping, warnings
// the accent colours the settings offer: name, CSS colour, and the same as RGB for the drawing code
// the text is 15 % bigger than it was drawn at first (a 5-inch screen)
const TEXT_SCALE: f64 = 1.15;

const ACCENTS: &[(&str, &str, (f64, f64, f64))] = &[
    ("Blue", "#00B1ED", (0.0, 0.694, 0.929)),
    ("Orange", "@accent", (1.0, 0.353, 0.122)),
    ("Green", "#3DDC84", (0.239, 0.863, 0.518)),
    ("Pink", "#FF4F9A", (1.0, 0.310, 0.604)),
    ("Amber", "#FFB02E", (1.0, 0.690, 0.180)),
    ("White", "#F2F2EE", (0.949, 0.949, 0.933)),
];
static ACCENT_IDX: AtomicUsize = AtomicUsize::new(0);
// high contrast is on (bright light, or asked for): the drawing code brightens its dim parts
static CONTRAST: AtomicBool = AtomicBool::new(false);

fn contrast() -> bool {
    CONTRAST.load(Ordering::Relaxed)
}

// the accent now (a setting; blue by default)
fn accent() -> (f64, f64, f64) {
    ACCENTS[ACCENT_IDX.load(Ordering::Relaxed).min(ACCENTS.len() - 1)].2
}

// the stylesheet for the accent @idx
fn css(idx: usize) -> String {
    format!("@define-color accent {};\n{}", ACCENTS[idx.min(ACCENTS.len() - 1)].1, scale_font_sizes(CSS, TEXT_SCALE))
}

// every "font-size: Npx" in @css scaled by @k
fn scale_font_sizes(css: &str, k: f64) -> String {
    let mut out = String::with_capacity(css.len() + 64);
    let mut rest = css;
    while let Some(i) = rest.find("font-size: ") {
        let (head, tail) = rest.split_at(i + "font-size: ".len());
        out.push_str(head);
        let end = tail.find("px").unwrap_or(0);
        match tail[..end].parse::<f64>() {
            Ok(v) => {
                out.push_str(&format!("{:.1}", v * k));
                rest = &tail[end..];
            }
            Err(_) => rest = tail,
        }
    }
    out.push_str(rest);
    out
}
const STRIP_LEN: f64 = 768.0;
// the right column's width, and what is left of it when the controls are stowed; the room the
// preview leaves for each (its margin), and the room under it for the lens strip
const RIGHT_W: f64 = 244.0;
const STOW_W: f64 = 100.0;
// a key's height, and the shutter's and the gallery's size when the controls are stowed
const KEY_H: f64 = 74.0;
const SHUTTER_W: f64 = 92.0;
const RESERVE_FULL: f64 = RIGHT_W + 10.0 + 12.0;
const RESERVE_STOW: f64 = STOW_W + 10.0 + 8.0;
const LENS_ROOM: f64 = 70.0;
// the room the overheating warning takes under the lens strip while it shows
const THERMAL_ROOM: f64 = 40.0;
// the flyout beside an encoder
const FLYOUT: (i32, i32) = (280, 84 + ruler::HEIGHT);
const HIST_BINS: usize = 64;

const CSS: &str = "
window.camera { background: #000; color: #f2f2ee; font-family: 'Adwaita Sans', 'Droid Sans', sans-serif; }
.mono, .set-value, .countdown, .burst-count { font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; }
.flyout { background: #101012; border: 1px solid alpha(@accent, 0.55); border-radius: 10px; }
.encoder.held { border-color: alpha(@accent, 0.8); background: #17171a; }
.encoder { background: #121214; border: 1px solid rgba(255,255,255,0.09); border-radius: 10px;
    transition: border-color 200ms ease, background 200ms ease; }
button.flat-white { background: none; border: none; box-shadow: none; outline: none; color: #f2f2ee;
    font-size: 15px; font-weight: 600; min-width: 44px; min-height: 40px; padding: 0; border-radius: 8px;
    transition: background 140ms ease, color 140ms ease; }
button.flat-white:active { background: rgba(255,255,255,0.10); }
.key { background: #151517; border: 1px solid rgba(255,255,255,0.09); box-shadow: none; outline: none;
    color: rgba(242,242,238,0.82); padding: 0; border-radius: 10px; min-width: 70px; min-height: 74px;
    font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace;
    transition: background 200ms ease, color 200ms ease, border-color 220ms ease, opacity 220ms ease, transform 160ms cubic-bezier(0.2, 0.8, 0.2, 1); }
.key:active { background: #2c2c32; transform: scale(0.93); }
.key.on { color: @accent; background: alpha(@accent, 0.13); border-color: alpha(@accent, 0.75); }
.zoom-pill { background: rgba(18,18,20,0.88); border: 1px solid rgba(255,255,255,0.09); border-radius: 8px; padding: 3px; }
.zoom-chip { background: none; border: none; box-shadow: none; outline: none; padding: 0; border-radius: 6px;
    color: rgba(242,242,238,0.75); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace;
    font-size: 16px; font-weight: 700; min-width: 70px; min-height: 42px;
    transition: background 140ms ease, color 140ms ease; }
.zoom-chip:active { transform: scale(0.94); }
.zoom-chip.active { background: @accent; color: #0b0b0c; }
.countdown { color: #f2f2ee; font-size: 110px; font-weight: 700; }
.thumb { border: 1px solid rgba(255,255,255,0.55); border-radius: 10px; }
.blackout { background: #000; }
.burst-screen { background: #000; }
.device-status label { color: rgba(242,242,238,0.75); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace;
    font-size: 15px; font-weight: 600; }
.battery-screen { background: #000; }
.thermal-warning { color: #fff; font-size: 14px; font-weight: 700; background: rgba(200,40,40,0.85);
    border-radius: 14px; padding: 5px 16px; }
.battery-screen label { color: #f2f2ee; font-size: 22px; font-weight: 600; }
.burst-count { color: #f2f2ee; font-size: 56px; }
.burst-saving { color: rgba(242,242,238,0.8); font-size: 20px; }
.assist-badge { color: @accent; font-size: 15px; }
.burst-badge { color: @accent; font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 12px;
    font-weight: 700; border: 1px solid alpha(@accent, 0.6); border-radius: 8px; padding: 0 6px; }
.settings { background: #000; }
.set-title { font-family: 'Adwaita Sans', 'Droid Sans', sans-serif; color: #f2f2ee; font-size: 17px; font-weight: 600; }
.set-sub { font-family: 'Adwaita Sans', 'Droid Sans', sans-serif; color: rgba(242,242,238,0.50); font-size: 13px; }
.spin { transition: transform 50ms ease-in; }
window.rot-cw .spin { transform: rotate(90deg); }
window.rot-ccw .spin { transform: rotate(-90deg); }
.fade { transition: opacity 180ms ease; }
.fade.off { opacity: 0; }
.picker { background: #141416; border: 1px solid alpha(@accent, 0.55); border-radius: 12px; padding: 6px; }
.pick-row { background: none; border: none; box-shadow: none; outline: none; color: rgba(242,242,238,0.85);
    border-radius: 8px; min-height: 54px; padding: 0 16px; font-size: 19px; font-weight: 600;
    transition: background 180ms ease, color 180ms ease, transform 140ms ease; }
.pick-row:active { background: rgba(255,255,255,0.14); transform: scale(0.97); }
.pick-row.on { background: @accent; color: #0b0b0c; }
.bubble { background: #141416; border: 1px solid alpha(@accent, 0.55); border-radius: 12px; padding: 12px 16px; }
.bubble-title { color: @accent; font-family: 'Adwaita Sans', 'Droid Sans', sans-serif; font-size: 17px; font-weight: 700; }
.bubble-text { color: rgba(242,242,238,0.82); font-family: 'Adwaita Sans', 'Droid Sans', sans-serif; font-size: 15px; }
.enc-wrap { transition: opacity 240ms ease, transform 280ms cubic-bezier(0.2, 0.8, 0.2, 1); }
.enc-wrap.gone { opacity: 0; transform: translateY(32px); }
.key.stowed { opacity: 0; transform: translateX(44px); }
.key.pinned { border-color: alpha(@accent, 0.55); }
.pill { background: #141416; border: 1px solid alpha(@accent, 0.55); border-radius: 999px; padding: 6px 18px;
    color: #f2f2ee; font-family: 'Adwaita Sans', 'Droid Sans', sans-serif; font-size: 16px; font-weight: 600; }
.alert-icon { color: @accent; }
.scrim { background: rgba(0,0,0,0.6); }
.sidebar { background: #101012; border-right: 1px solid rgba(255,255,255,0.12); padding: 22px 18px;
    transition: transform 260ms cubic-bezier(0.2, 0.8, 0.2, 1); }
.sidebar.side-hidden { transform: translateX(-340px); }
.side-title { color: rgba(242,242,238,0.5); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 13px;
    font-weight: 700; letter-spacing: 3px; margin-bottom: 8px; }
.side-row { background: none; border: none; box-shadow: none; outline: none; color: #f2f2ee; border-radius: 10px;
    min-height: 56px; padding: 0 12px; font-size: 19px; font-weight: 600; }
.side-row:active { background: rgba(255,255,255,0.14); transform: scale(0.98); }
.side-head { color: rgba(242,242,238,0.45); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 12px;
    font-weight: 700; letter-spacing: 2px; margin-top: 18px; margin-bottom: 4px; padding-left: 12px; }
.side-quick { padding-left: 12px; }
.side-about { color: rgba(242,242,238,0.42); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 12px; padding-left: 12px; }
.settings { background: #000; }
.nav { background: #0b0b0c; }
.nav row { padding: 20px 26px; background: none; color: rgba(242,242,238,0.7); font-size: 19px; font-weight: 600;
    border-bottom: 1px solid rgba(255,255,255,0.06); }
.nav row:selected { background: alpha(@accent, 0.14); color: @accent; }
.pane { padding: 18px 34px 34px 34px; }
.pane-title { color: rgba(242,242,238,0.45); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 12px;
    font-weight: 700; letter-spacing: 3px; margin-bottom: 8px; }
.pane-row { padding: 16px 0; border-bottom: 1px solid rgba(255,255,255,0.08); }
.choices { margin-top: 2px; }
.about-line { color: rgba(242,242,238,0.6); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 15px; margin-top: 6px; }
.check-label { color: #f2f2ee; font-size: 17px; font-weight: 600; }
checkbutton { padding: 6px 0; }
checkbutton check { margin-right: 12px; min-width: 24px; min-height: 24px; border-radius: 7px; background: #17171a;
    border: 1px solid rgba(255,255,255,0.28); -gtk-icon-source: none; }
checkbutton check:checked { background: @accent; border-color: @accent; }
checkbutton radio { margin-right: 12px; min-width: 24px; min-height: 24px; border-radius: 999px; background: #17171a;
    border: 1px solid rgba(255,255,255,0.28); -gtk-icon-source: none; }
checkbutton radio:checked { background: @accent; border-color: @accent; }
window.contrast .encoder, window.contrast .key, window.contrast .zoom-pill { background: #26262b; border-color: rgba(255,255,255,0.4); }
window.contrast .key { color: #ffffff; }
window.contrast .key.on { background: alpha(@accent, 0.28); border-color: @accent; }
window.contrast .zoom-chip { color: #ffffff; }
window.contrast .pill, window.contrast .bubble { background: #26262b; border-width: 2px; }
window.contrast .bubble-text { color: #ffffff; }
window.contrast .sidebar, window.contrast .nav { background: #1c1c20; }
window.contrast .set-sub, window.contrast .side-about, window.contrast .about-line { color: rgba(255,255,255,0.85); }
.page { transition: opacity 240ms ease, transform 280ms cubic-bezier(0.2, 0.8, 0.2, 1); }
.page.page-off { opacity: 0; transform: translateX(56px); }
.nav row { transition: background 180ms ease, color 180ms ease; }
.side-row, .pick-row, .zoom-chip { transition: background 180ms ease, color 180ms ease, transform 140ms ease; }
";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Auto,
    Iso,     // ISO priority: the ASICs choose the shutter
    Shutter, // shutter priority: they choose the ISO
    Manual,
}

// stock's modes in its mode wheel's order (CameraMode; its video mode aside)
const MODES: [Mode; 4] = [Mode::Auto, Mode::Iso, Mode::Shutter, Mode::Manual];

impl Mode {
    fn index(self) -> usize {
        MODES.iter().position(|&m| m == self).unwrap_or(0)
    }

    // the toolbar opener's (and the settings file's)
    fn short(self) -> &'static str {
        ["auto", "iso", "shutter", "manual"][self.index()]
    }

    fn fixes_iso(self) -> bool {
        matches!(self, Mode::Iso | Mode::Manual)
    }

    fn fixes_shutter(self) -> bool {
        matches!(self, Mode::Shutter | Mode::Manual)
    }
}

// a photo's way from the shutter to the LRI (threads report on App::stage_tx)
enum Stage {
    Captured(Result<PathBuf, String>), // the ASICs hold it (records in DIR)
    Transferred(Result<PathBuf, (PathBuf, String)>), // in DIR/asic*.raw (or DIR and what failed)
    Saved(Result<PathBuf, String>),       // the LRI
}

#[derive(Clone, Copy, PartialEq)]
enum Dial {
    Iso,
    Shutter,
    Ev,
}

struct State {
    mode: Mode,
    iso: f64,     // position, see iso_at
    shutter: f64, // position, see secs_at
    ev: f64,      // position, see ev_at
    zoom: f64,
    module: usize,
    timer: usize,
    grid: u8, // 0 off, 1 3x3, 2 golden ratio
    histogram: bool,
    assist: u8, // focus peaking (1) and zebras (2), as bits
    accent: usize, // ACCENTS' index
    contrast: usize, // high contrast: 0 auto (in bright light), 1 on, 2 off
    sparkle: bool, // a burst of light from the LED by the shutter button when a photo is taken
    pinned: u16, // the keys that stay when the controls are swiped away (a bit each, grid order)
    strip_fn: usize, // what the touch strip does: 0 zoom, 1 ISO, 2 shutter, 3 EV
    strip_set: u8,   // which of those a double tap cycles through (a bit each)
    strip_tap_at: Option<Instant>, // the last tap on the strip's middle
    busy: bool,
    counting: bool,
    saving: u32,
    seq: u32,
    burst_count: u8,  // the burst screen's number (0: not showing)
    burst_captured: bool,
    burst: usize,
    flash: u8, // 0 off, 1 auto, 2 on
    wb: usize,  // wb::PRESETS
    dragged: bool,
    wheel: Option<Dial>,
    wheel_start: f64,
    // closes the exposure wheel after a drag; a new drag cancels it
    wheel_close: Option<glib::SourceId>,
    haptics: u8, // 0 off, 1 normal, 2 strong (stock's)
    continuous: bool, // ISO and shutter anywhere, rather than stock's 1/3-stop list
    zoom_start: f64,
    zoom_raw: f64, // where the gesture has taken the zoom, before it snaps to a prime
    zoom_wheel_until: Option<Instant>,
    focus_until: Option<Instant>,
    // the focus marks' run: when it started, and its outcome (1 focused, 2 not) and when
    focus_t0: Option<Instant>,
    focus_done: Option<(i32, Instant)>,
    focus_at: Option<(f64, f64)>,
    zoom_sent: Instant,
    // AF-D: the zoom at the last focus; no refocusing until then after a focus by hand
    caf_zoom: Option<f64>,
    caf_pause_until: Option<Instant>,
    // the settings screen's
    metering: u8, // 0 centre-weighted, 1 touch, 2 whole frame
    caf: bool,
    stacked: bool,
    exposure_info: bool,
    inverse_wheel: bool,
    strip_zoom: bool,
    tools: Vec<Tool>, // the toolbar's buttons, in order
    tool_cycle: bool, // a button with choices steps through them, rather than showing them
    asleep: bool, // the preview stopped while it can't be seen (follow_screen)
    unseen_since: Option<Instant>,
    screen_off: bool,
    fast_loop_on: bool,
    tripod: bool, // tripod mode as last sent
    polls: u32,
    lens_warning: u8, // the settings screen's: a covered lens: 0 nothing, 1 the warning, 2 and a buzz
    lens_mask: u8,      // the covered sensors last shown
    // stock's device status: shown (the settings screen's), the battery (level, charging),
    // captures left, and the low storage warning given (0 none, 1 captures, 2 space)
    device_status: bool,
    battery: (u8, bool),
    battery_low: bool,
    captures_left: u64,
    storage_warned: u8,
    // stock's in-pocket check: on (the settings screen's), and since when it has looked so
    pocket: bool,
    pocket_since: Option<Instant>,
    // where photos are taken, in them (geo.rs)
    geotag: bool,
    // stock's thermal levels from the camera modules' temperature: 0 safe, 1 warm (55 C,
    // clear under 45), 2 hot (65 C, clear under 56); while hot the preview is stopped
    // (cooling) until this, then started again for a new reading
    thermal: u8,
    thermal_pause_until: Option<Instant>,
    live_iso: i32,
    live_secs: f64,
    strip_down: bool,
    strip_active: bool,            // the finger has gone past the dead zone: a slide
    strip_lock: usize,             // the function this touch began with
    strip_slid_at: Option<Instant>, // when the last slide ended
    strip_x0: i32,
    strip_x: i32,
    strip_t0: Instant,
    settle: Option<glib::SourceId>,
    switching: Option<mpsc::Receiver<usize>>,
}

impl State {
    // what's kept between runs, as the settings file's lines
    fn saved(&self) -> String {
        let mode = self.mode.short();
        format!(
            "mode={mode}\niso={}\nshutter={}\nev={}\nflash={}\ntimer={}\ngrid={}\nhistogram={}\nassist={}\nburst={}\n\
             wb={}\nmetering={}\ncaf={}\nstacked={}\nexposure_info={}\ninverse_wheel={}\nhaptics={}\ncontinuous={}\nstrip_zoom={}\ntoolbar={}\ntool_cycle={}\nlens_warn={}\ndevice_status={}\npocket={}\ngeotag={}\naccent={}\ncontrast={}\npinned={}\nstrip_set={}\nsparkle={}\n",
            self.iso,
            self.shutter,
            self.ev,
            self.flash,
            self.timer,
            self.grid,
            self.histogram as u8,
            self.assist,
            self.burst,
            self.wb,
            self.metering,
            self.caf as u8,
            self.stacked as u8,
            self.exposure_info as u8,
            self.inverse_wheel as u8,
            self.haptics,
            self.continuous as u8,
            self.strip_zoom as u8,
            self.tools.iter().map(|t| t.name()).collect::<Vec<_>>().join(","),
            self.tool_cycle as u8,
            self.lens_warning,
            self.device_status as u8,
            self.pocket as u8,
            self.geotag as u8,
            self.accent,
            self.contrast,
            self.pinned,
            self.strip_set,
            self.sparkle as u8,
        )
    }

    fn load(&mut self, m: &std::collections::HashMap<String, String>) {
        let num = |k: &str| m.get(k).and_then(|v| v.parse::<f64>().ok());
        let flag = |k: &str, d: bool| num(k).map_or(d, |v| v != 0.0);
        if let Some(&mode) = MODES.iter().find(|md| m.get("mode").map(String::as_str) == Some(md.short())) {
            self.mode = mode;
        }
        self.ev = num("ev").unwrap_or(self.ev).clamp(0.0, 1.0);
        self.iso = num("iso").unwrap_or(self.iso).clamp(0.0, 1.0);
        self.shutter = num("shutter").unwrap_or(self.shutter).clamp(0.0, 1.0);
        self.flash = num("flash").map_or(self.flash, |v| (v as u8).min(2));
        self.timer = num("timer").map_or(self.timer, |v| (v as usize).min(TIMERS.len() - 1));
        self.grid = num("grid").map_or(self.grid, |v| (v as u8).min(2));
        self.histogram = flag("histogram", self.histogram);
        self.assist = num("assist").map_or(self.assist, |v| (v as u8).min(3));
        self.burst = num("burst").map_or(self.burst, |v| (v as usize).min(BURSTS.len() - 1));
        self.metering = num("metering").map_or(self.metering, |v| (v as u8).min(2));
        self.caf = flag("caf", self.caf);
        self.wb = num("wb").map_or(self.wb, |v| (v as usize).min(wb::PRESETS.len() - 1));
        self.stacked = flag("stacked", self.stacked);
        self.exposure_info = flag("exposure_info", self.exposure_info);
        self.inverse_wheel = flag("inverse_wheel", self.inverse_wheel);
        self.haptics = num("haptics").map_or(self.haptics, |v| (v as u8).min(2));
        self.continuous = flag("continuous", self.continuous);
        self.strip_zoom = flag("strip_zoom", self.strip_zoom);
        if let Some(t) = m.get("toolbar") {
            self.tools = t.split(',').filter_map(|n| TOOLS.iter().copied().find(|t| t.name() == n)).collect();
            self.tools.dedup();
        }
        self.tool_cycle = flag("tool_cycle", self.tool_cycle);
        self.lens_warning = num("lens_warn").map_or(self.lens_warning, |v| (v as u8).min(2));
        self.device_status = flag("device_status", self.device_status);
        self.pocket = flag("pocket", self.pocket);
        self.geotag = flag("geotag", self.geotag);
        self.sparkle = flag("sparkle", self.sparkle);
        self.contrast = num("contrast").map_or(self.contrast, |v| (v as usize).min(2));
        self.pinned = num("pinned").map_or(self.pinned, |v| v as u16 & 0x0fff);
        self.strip_set = num("strip_set").map_or(self.strip_set, |v| (v as u8) & 0b1111);
        self.accent = num("accent").map_or(self.accent, |v| (v as usize).min(ACCENTS.len() - 1));
    }
}

// the keys of the grid (the mode key, which opens its picker, is apart)
#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Flash,
    Wb,
    Timer,
    Grid,
    Histogram,
    Burst,
    Assist,
    Afd,
    Meter,
    Geo,
    Strip,
}

const TOOLS: [Tool; 11] = [
    Tool::Flash,
    Tool::Wb,
    Tool::Timer,
    Tool::Grid,
    Tool::Histogram,
    Tool::Assist,
    Tool::Burst,
    Tool::Afd,
    Tool::Meter,
    Tool::Geo,
    Tool::Strip,
];

impl Tool {
    // in the settings file
    fn name(self) -> &'static str {
        match self {
            Tool::Flash => "flash",
            Tool::Wb => "wb",
            Tool::Timer => "timer",
            Tool::Grid => "grid",
            Tool::Histogram => "histogram",
            Tool::Burst => "burst",
            Tool::Assist => "assist",
            Tool::Afd => "afd",
            Tool::Meter => "meter",
            Tool::Geo => "geo",
            Tool::Strip => "strip",
        }
    }

    // its choices, when it has more than two
    fn opt(self) -> Option<Opt> {
        match self {
            Tool::Flash => Some(Opt::Flash),
            Tool::Wb => Some(Opt::Wb),
            Tool::Timer => Some(Opt::Timer),
            Tool::Grid => Some(Opt::Grid),
            Tool::Burst => Some(Opt::Burst),
            Tool::Assist => Some(Opt::Assist),
            Tool::Meter => Some(Opt::Meter),
            Tool::Histogram | Tool::Afd | Tool::Geo | Tool::Strip => None,
        }
    }
}

// the toolbar's settings with more than two choices
#[derive(Clone, Copy, PartialEq)]
enum Opt {
    Flash,
    Wb,
    Timer,
    Grid,
    Burst,
    Assist,
    Meter,
}

struct App {
    st: RefCell<State>,
    ccb: Option<ccb::Ccb>,
    focusing: Arc<AtomicBool>,
    stage_tx: mpsc::Sender<Stage>,
    ctl_tx: mpsc::Sender<(u32, i32)>,
    // the driver's metered ISO and exposure (us), the photo's as the ASICs have it, and the
    // preview's digital boost x 100, read by a thread of their own
    metered: Arc<[std::sync::atomic::AtomicI32; 4]>,
    // tripod mode: the gyro read while the preview runs, and whether the camera is still
    gyro_on: Arc<AtomicBool>,
    focal: Arc<AtomicU32>, // the zoom (35 mm focal length x 10), for the gyro's blur limit
    still: Arc<AtomicBool>,
    // AF-D: the camera has moved and settled since the last look (the gyro thread)
    moved: Arc<AtomicBool>,
    // the proximity sensors covered (a bit each, prox's thread), and stock's warning
    blocked: Arc<AtomicU8>,
    // stock's device status (top left) and its battery-low screen
    status_box: gtk::Box,
    // iio-sensor-proxy, for the ambient light (claimed while the app runs)
    light: Option<gtk::gio::DBusProxy>,
    // and the accelerometer's orientation, for portrait: the UI's quarter turns clockwise
    // (-1, 0, 1; as stock, none upside down) and what turns with it re-laid out
    accel: Option<gtk::gio::DBusProxy>,
    quarter: Cell<i32>,
    // the status line, at the preview's top edge as the camera is held (place_status), and
    // the overheating warning at its bottom edge
    // the notices at the top of the screen: a small pill (icon and title) and an alert (icon, title
    // and a line) under it
    pill_turn: Rotator,
    pill_label: gtk::Label,
    pill_timer: RefCell<Option<glib::SourceId>>,
    alert_turn: Rotator,
    alert_icon: gtk::Label,
    lens_art: gtk::DrawingArea,
    alert_title: gtk::Label,
    alert_text: gtk::Label,
    alert_timer: RefCell<Option<glib::SourceId>>,
    alert_key: Cell<&'static str>,
    zoom_pill: gtk::Box,
    thermal_t: Cell<f64>,
    thermal_anim: RefCell<Option<gtk::TickCallbackId>>,
    thermal_turn: Rotator,
    // the lens strip (the primes as keys, the zoom on the nearest) under the preview
    zoom_chips: Vec<gtk::Button>,
    // in front: the screen kept on (an idle inhibitor's cookie) and the display held in
    // landscape (the rotation lock and transform it had, given back after)
    idle_cookie: Cell<u32>,
    landscape_held: RefCell<Option<(bool, Option<String>)>>,
    rotators: Vec<Rotator>,
    geo: RefCell<geo::Geo>,
    storage_label: gtk::Label,
    battery_label: gtk::Label,
    battery_screen: gtk::Box,
    hot_screen: gtk::Box,
    transfers: RefCell<Option<transfer::Transfers>>,
    // the transfers wait for the preview's first frame (start_transfers_on_frame)
    transfers_wait: RefCell<Option<glib::SignalHandlerId>>,
    transfer_turn: Arc<Mutex<()>>,
    stage_rx: mpsc::Receiver<Stage>,
    input_rx: mpsc::Receiver<input::Ev>,
    pipeline: gst::Pipeline,
    paintable: gdk::Paintable,
    _bus: gst::bus::BusWatchGuard,
    view: ZoomView,
    marks: Canvas,
    hist_area: Canvas,
    wheels: Ruler,
    // the three encoders (ISO, shutter, EV) and what each was last drawn for (refresh)
    encoders: Vec<Canvas>,
    enc_shown: Cell<[u64; 3]>,
    // the big readout that opens beside an encoder while it is held, out from under the thumb
    flyout: Canvas,
    flyout_turn: Rotator,
    enc_roll: Vec<Rc<Roll>>,
    flyout_roll: Rc<Roll>,
    flyout_cells: Cell<usize>,
    bright: Cell<bool>, // the light is bright enough for high contrast (with some hysteresis)
    shutter_flash: Cell<Option<Instant>>,
    flyout_unit: Cell<&'static str>,
    flyout_suffix: Cell<&'static str>, // after the number: "mm"
    // where the flyout was last placed (0-2 an encoder, 3 the zoom, 255 hidden)
    flyout_at: Cell<u8>,
    css: gtk::CssProvider,
    root: gtk::Overlay,
    shutter: Canvas,
    thumb: gtk::Image,
    thumb_spin: gtk::DrawingArea,
    blackout: gtk::Box,
    burst_screen: gtk::Box,
    burst_label: gtk::Label,
    burst_saving: gtk::Box,
    burst_dots: gtk::DrawingArea,
    burst_badge: gtk::Label,
    // stock's assist icons: tripod mode on (the camera still), a stacked capture ahead (the
    // moon, stock's "low-light assist")
    tripod_badge: gtk::Label,
    // the last focus run's outcome, from its thread: 0 running, 1 focused, 2 not
    af_outcome: Arc<std::sync::atomic::AtomicI32>,
    // the focus marks are being animated; the grid as last drawn
    marks_ticking: Cell<bool>,
    focus_area: gtk::DrawingArea,
    // the focus point in the focus layer
    focus_centre: Cell<(f64, f64)>,
    marks_grid: Cell<u8>,
    // whether the shutter button was drawn busy (refresh)
    dials_shown: Cell<Option<bool>>,
    moon_badge: gtk::Label,
    shake_badge: gtk::Label,
    preview_gain: Cell<f32>,
    // the mode key (its picker opens beside it), and the picker's rows in MODES' order
    mode_btn: gtk::Button,
    // the keys in the grid's order, whether the controls are swiped away, and the system panel
    keys: Vec<gtk::Button>,
    key_grid: gtk::Grid,
    pin_strip: gtk::Box,
    deck: gtk::Overlay,
    centre: gtk::Overlay,
    frame: gtk::AspectFrame,
    right: gtk::Box,
    shutter_row: gtk::Box,
    shutter_fill: gtk::Box,
    stow_t: Cell<f64>,
    stow_anim: RefCell<Option<gtk::TickCallbackId>>,
    compact: Cell<bool>,
    stowed: Cell<bool>,
    picker_turn: Rotator,
    picker_card: gtk::Box,
    picker_rows: Vec<gtk::Button>,
    picker_timer: RefCell<Option<glib::SourceId>>,
    // the bubble that explains a choice, and the encoders' turned wrappers (EV hides in manual)
    enc_turn: Vec<Rotator>,
    meter_btn: gtk::Button,
    geo_btn: gtk::Button,
    strip_btn: gtk::Button,
    timer_btn: gtk::Button,
    grid_btn: gtk::Button,
    hist_btn: gtk::Button,
    hist: RefCell<Vec<u32>>,
    // when each hint (a badge's description) was last shown, so one that flickers is said once
    hint_at: RefCell<HashMap<&'static str, Instant>>,
    burst_btn: gtk::Button,
    flash_btn: gtk::Button,
    wb_btn: gtk::Button,
    afd_btn: gtk::Button,
    assist_btn: gtk::Button,
    // the settings screen, and the list a value is chosen from over it
    // the settings screen: its categories and the pane of the one shown
    settings_nav: gtk::ListBox,
    settings_pane: gtk::Box,
    // the sidebar: its panel, the dimmed layer behind it, its quick checkboxes and its About lines
    side_panel: gtk::Box,
    scrim: gtk::Box,
    side_quick: gtk::Box,
    side_about: gtk::Label,
    cal: wb::Calibration,
    motor: haptics::Haptics,
    // a photo's view preferences for the LRI (white balance, exposure), by its directory
    photo_args: RefCell<HashMap<PathBuf, Vec<String>>>,
    settings_page: gtk::Overlay,
    last_saved: RefCell<String>,
    // logind's sleep inhibitor while photos are on their way (dropped: released)
    sleep_inhibitor: RefCell<Option<std::os::fd::OwnedFd>>,
    countdown: gtk::Label,
}

// what the touch strip does: its functions (the double tap cycles through those chosen in the
// settings, which the current mode allows), as the key's caption and the bubble's title
const STRIP_FNS: [(&str, &str); 4] = [("ZOOM", "Zoom"), ("ISO", "ISO"), ("TIME", "Shutter"), ("EV", "Exposure compensation")];

const MODE_NAMES: [(&str, &str); 4] = [
    ("Auto", "The camera chooses everything; you can still adjust EV."),
    ("ISO priority", "You set the ISO; the camera picks the shutter."),
    ("Shutter priority", "You set the shutter; the camera picks the ISO."),
    ("Manual", "You set the ISO and the shutter."),
];

const ISO_MAX: f64 = 3200.0;
const ISO_ANALOG_MAX: f64 = 775.0; // stock's analog ceiling: 7.75x
const ISO_MIN: f64 = 100.0;
const SECS_MAX: f64 = 15.0;
const SECS_MIN: f64 = 1.0 / 8000.0;

fn iso_at(pos: f64) -> i32 {
    (ISO_MAX * (ISO_MIN / ISO_MAX).powf(pos.clamp(0.0, 1.0))).round() as i32
}

fn secs_at(pos: f64) -> f64 {
    SECS_MAX * (SECS_MIN / SECS_MAX).powf(pos.clamp(0.0, 1.0))
}

fn iso_pos(iso: f64) -> f64 {
    (iso / ISO_MAX).ln() / (ISO_MIN / ISO_MAX).ln()
}

fn secs_pos(t: f64) -> f64 {
    (t / SECS_MAX).ln() / (SECS_MIN / SECS_MAX).ln()
}

// EV compensation in thirds: +3 EV at the top of the wheel (position 0), -3 at the bottom
fn ev_at(pos: f64) -> i32 {
    (9.0 - 18.0 * pos.clamp(0.0, 1.0)).round() as i32
}

fn ev_pos(ev: i32) -> f64 {
    (9 - ev) as f64 / 18.0
}

fn fmt_ev(ev: i32) -> String {
    if ev == 0 {
        return "0".into();
    }
    let sign = if ev > 0 { "+" } else { "-" };
    let (whole, third) = (ev.abs() / 3, ["", "⅓", "⅔"][(ev.abs() % 3) as usize]);
    if whole == 0 {
        format!("{sign}{third}")
    } else {
        format!("{sign}{whole}{third}")
    }
}

fn shutter_secs(s: &str) -> f64 {
    match s.strip_prefix("1/") {
        Some(d) => 1.0 / d.parse::<f64>().unwrap_or(1.0),
        None => s.parse().unwrap_or(1.0),
    }
}

fn fmt_secs(t: f64) -> String {
    if t >= 0.3 {
        let s = format!("{t:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        format!("1/{:.0}", 1.0 / t)
    }
}

// a dial's list values (its tick marks) as positions: stock's ISO and shutter lists, EV thirds
fn dial_ticks(dial: Dial) -> Vec<f64> {
    match dial {
        Dial::Iso => ISO.iter().map(|&i| iso_pos(i as f64)).collect(),
        Dial::Shutter => SHUTTER.iter().map(|s| secs_pos(shutter_secs(s))).collect(),
        Dial::Ev => (-9..=9).map(ev_pos).collect(),
    }
}

// the list value nearest @pos on a dial
fn dial_tick(dial: Dial, pos: f64) -> usize {
    let ticks = dial_ticks(dial);
    let mut best = 0;
    for (i, t) in ticks.iter().enumerate() {
        if (t - pos).abs() < (ticks[best] - pos).abs() {
            best = i;
        }
    }
    best
}

// the zoom wheel's dot nearest @zoom (31 dots, 28 to 150 mm, even in log)
fn zoom_dot(zoom: f64) -> i64 {
    (30.0 * (zoom / ZOOM_MIN).ln() / (ZOOM_MAX / ZOOM_MIN).ln()).round() as i64
}

fn preview_module(zoom: f64) -> usize {
    usize::from(zoom >= 70.0)
}

fn module_for(zoom: f64) -> usize {
    if zoom < 70.0 {
        0
    } else if zoom < 150.0 {
        1
    } else {
        2
    }
}

// text at (x, y), vertically centred; align 0 = left, 0.5 = centre, 1 = right
// (through Pango, which falls back to other fonts for glyphs like ⅓ that cairo's own text
// drew as boxes)
fn text(cr: &cairo::Context, s: &str, x: f64, y: f64, size: f64, align: f64) {
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = gtk::pango::FontDescription::from_string("Adwaita Mono, Droid Sans Mono, Monospace Bold");
    font.set_absolute_size(size * TEXT_SCALE * gtk::pango::SCALE as f64);
    layout.set_font_description(Some(&font));
    layout.set_text(s);
    let (ink, _) = layout.pixel_extents();
    cr.move_to(
        x - ink.width() as f64 * align - ink.x() as f64,
        y - ink.height() as f64 / 2.0 - ink.y() as f64,
    );
    pangocairo::functions::show_layout(cr, &layout);
}

// @r centred on the preview's top (or bottom) edge as the camera is held, @margin from it:
// turned -90 (shutter down) the top is the left edge, turned 90 the right
fn place_on_edge(r: &Rotator, q: i32, top: bool, margin: i32) {
    let side = if top { q } else { -q };
    let (h, v) = match (q, top) {
        (0, true) => (gtk::Align::Center, gtk::Align::Start),
        (0, false) => (gtk::Align::Center, gtk::Align::End),
        _ if side == -1 => (gtk::Align::Start, gtk::Align::Center),
        _ => (gtk::Align::End, gtk::Align::Center),
    };
    r.set_halign(h);
    r.set_valign(v);
    r.set_margin_top(if q == 0 && top { margin } else { 0 });
    r.set_margin_bottom(if q == 0 && !top { margin } else { 0 });
    r.set_margin_start(if q != 0 && side == -1 { margin } else { 0 });
    r.set_margin_end(if q != 0 && side == 1 { margin } else { 0 });
}


// what an encoder shows: its name, the value, where that sits in its range (0 low, 1 high: a
// greater position is a lower value), and whether the mode has it in hand
fn encoder_shows(st: &State, dial: Dial) -> (&'static str, String, f64, bool) {
    match dial {
        Dial::Iso => {
            let active = st.mode.fixes_iso();
            let (v, pos) = if active { (iso_at(st.iso), st.iso) } else { (st.live_iso, iso_pos(st.live_iso.max(1) as f64)) };
            ("ISO", if v > 0 { v.to_string() } else { "-".into() }, 1.0 - pos, active)
        }
        Dial::Shutter => {
            let active = st.mode.fixes_shutter();
            let secs = if active { secs_at(st.shutter) } else { st.live_secs };
            let pos = if active { st.shutter } else { secs_pos(secs.max(SECS_MIN)) };
            ("SHUTTER", if secs > 0.0 { fmt_secs(secs) } else { "-".into() }, 1.0 - pos, active)
        }
        Dial::Ev => {
            let active = st.mode != Mode::Manual;
            ("EV", if active { fmt_ev(ev_at(st.ev)) } else { "-".into() }, 1.0 - st.ev, active)
        }
    }
}

// a change-detection key for an encoder's drawing
fn encoder_key(st: &State, dial: Dial) -> u64 {
    use std::hash::{Hash, Hasher};
    let (name, text, frac, active) = encoder_shows(st, dial);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (name, text, (frac * 1000.0) as i64, active).hash(&mut h);
    h.finish()
}

// @s centred on (x, y) in the current font and source
fn text_at(cr: &cairo::Context, s: &str, x: f64, y: f64, size: f64) {
    text(cr, s, x, y, size, 0.5);
}

fn dial_index(dial: Dial) -> usize {
    match dial {
        Dial::Iso => 0,
        Dial::Shutter => 1,
        Dial::Ev => 2,
    }
}

fn dial_name(dial: Dial) -> &'static str {
    ["ISO", "SHUTTER", "EV"][dial_index(dial)]
}

fn set_class(w: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        w.add_css_class(class);
    } else {
        w.remove_css_class(class);
    }
}

// the number line's scale for an exposure value: its list's entries, labelled where there is room
// (every ISO; the whole stops of shutter and EV); the higher values to the right
fn exposure_spec(dial: Dial) -> ruler::Spec {
    let tick = |pos: f64, label: Option<String>| ruler::Tick { pos, label };
    let ticks = match dial {
        Dial::Iso => ISO.iter().map(|&i| tick(iso_pos(i as f64), Some(i.to_string()))).collect(),
        Dial::Shutter => SHUTTER
            .iter()
            .enumerate()
            .map(|(k, s)| tick(secs_pos(shutter_secs(s)), (k % 3 == 0).then(|| s.to_string())))
            .collect(),
        Dial::Ev => (-9..=9).map(|e| tick(ev_pos(e), (e % 3 == 0).then(|| fmt_ev(e)))).collect(),
    };
    let px_per_unit = match dial {
        Dial::Iso => 520.0,
        Dial::Shutter => 1300.0,
        Dial::Ev => 560.0,
    };
    ruler::Spec { ticks, px_per_unit, dir: 1.0 }
}

// the zoom's: focal lengths on a log scale, the primes labelled (they are what the zoom snaps to)
fn zoom_spec() -> ruler::Spec {
    const MM: [f64; 11] = [28.0, 30.0, 35.0, 40.0, 50.0, 60.0, 70.0, 85.0, 100.0, 120.0, 150.0];
    let ticks = MM
        .iter()
        .map(|&mm| ruler::Tick {
            pos: (mm / ZOOM_MIN).ln(),
            label: PRIMES.contains(&mm).then(|| format!("{mm:.0}")),
        })
        .collect();
    ruler::Spec { ticks, px_per_unit: 230.0, dir: -1.0 }
}

fn make_pipeline() -> (gst::Pipeline, gdk::Paintable) {
    if dev::demo() {
        return (gst::Pipeline::new(), dev::demo_paintable());
    }
    // the frames go to the display as they are, DMA-BUFs imported by GTK (our libcamerasrc
    // offers them: libcamera patch 0007): no CPU copy or conversion per frame. (A copy into
    // buffers of our own used to keep the display off libcamera's memory, freed when the
    // camera stopped; an imported DMA-BUF holds its own reference.) The plain BGRx caps are
    // for libcamerasrc's first query, which only knows system memory; it then offers DMA-BUF.
    let pipeline = gst::parse::launch(
        "libcamerasrc name=src \
         ! video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=XR24,width=1024,height=768; \
           video/x-raw,format=BGRx,width=1024,height=768 \
         ! queue max-size-buffers=1 leaky=downstream ! gtk4paintablesink name=sink",
    )
    .expect("preview pipeline")
    .downcast::<gst::Pipeline>()
    .expect("a pipeline");
    let sink = pipeline.by_name("sink").expect("sink");
    let paintable = sink.property::<gdk::Paintable>("paintable");
    (pipeline, paintable)
}

impl App {
    fn start_preview(self: &Rc<Self>) {
        // not before the last camera's streams are stopped (take_camera)
        if !CAMERA_TAKEN.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.pipeline.set_state(gst::State::Playing);
    }

    fn stop_preview(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }

    fn exposure_us(&self) -> i32 {
        let st = self.st.borrow();
        ((secs_at(st.shutter) * 1e6).round() as i32).clamp(1, 15_000_000)
    }

    // auto: the ASICs meter; the priority modes: they meter the other half; manual: the chosen
    // ISO and shutter (the preview slows down for long shutters, as stock's)
    fn apply_exposure(&self) {
        let (mode, iso, ev) = {
            let st = self.st.borrow();
            (st.mode, iso_at(st.iso), ev_at(st.ev))
        };
        let _ = self.ctl_tx.send((ccb::EV, ev));
        if mode.fixes_iso() {
            let _ = self.ctl_tx.send((ccb::ISO, iso));
        }
        if mode.fixes_shutter() {
            let _ = self.ctl_tx.send((ccb::EXPOSURE_US, self.exposure_us()));
        }
        let priority = match mode {
            Mode::Iso => 1,
            Mode::Shutter => 2,
            _ => 0,
        };
        let _ = self.ctl_tx.send((ccb::PRIORITY, priority));
        let _ = self.ctl_tx.send((ccb::EXPOSURE_AUTO, (mode == Mode::Manual) as i32));
    }

    fn refresh(&self) {
        let st = self.st.borrow();
        let iso = if st.mode.fixes_iso() { iso_at(st.iso) } else { st.live_iso };
        // stock keeps the sensors' analog gain at most 7.75 (ISO 775) and has its ISP apply
        // the ASICs' digital boost on top, before its gamma: the software ISP does the same
        // here (libcamera's DigitalGain, our patch). With the ISO set (ISO priority, manual)
        // the rest of that ISO; otherwise the boost the ASICs report, which in shutter
        // priority brightens the preview towards a long exposure the preview can't take.
        let gain = if st.mode.fixes_iso() {
            (iso as f64 / ISO_ANALOG_MAX).clamp(1.0, ISO_MAX / ISO_ANALOG_MAX) as f32
        } else {
            (self.metered[2].load(Ordering::Relaxed) as f32 / 100.0).clamp(1.0, 32.0)
        };
        if (gain - self.preview_gain.get()).abs() > 0.005 {
            self.preview_gain.set(gain);
            if let Some(src) = self.pipeline.by_name("src") {
                src.set_property("digital-gain", gain);
            }
        }
        // the lens pill: the nearest prime shows the zoom, as a phone's does between lenses
        let near = (0..PRIMES.len())
            .min_by(|&a, &b| (PRIMES[a] - st.zoom).abs().total_cmp(&(PRIMES[b] - st.zoom).abs()))
            .unwrap_or(0);
        for (i, chip) in self.zoom_chips.iter().enumerate() {
            if i == near {
                chip.set_label(&format!("{:.0}", st.zoom));
                chip.add_css_class("active");
            } else {
                chip.set_label(&format!("{:.0}", PRIMES[i]));
                chip.remove_css_class("active");
            }
        }
        self.focal.store((st.zoom * 10.0) as u32, Ordering::Relaxed);
        let t = TIMERS[st.timer];
        icons::set_key(&self.timer_btn, if t == 0 { icons::TIMER_OFF } else { icons::TIMER }, &if t == 0 { "OFF".to_string() } else { format!("{t}S") });
        if t == 0 {
            self.timer_btn.remove_css_class("on");
        } else {
            self.timer_btn.add_css_class("on");
        }
        icons::set_key(&self.grid_btn, if st.grid == 0 { icons::GRID_OFF } else { icons::GRID }, ["OFF", "3X3", "PHI"][st.grid as usize]);
        icons::set_key(&self.hist_btn, icons::HISTOGRAM, if st.histogram { "ON" } else { "OFF" });
        if st.histogram {
            self.hist_btn.add_css_class("on");
        } else {
            self.hist_btn.remove_css_class("on");
        }
        icons::set_key(&self.flash_btn, [icons::FLASH_OFF, icons::FLASH_AUTO, icons::FLASH][st.flash as usize], ["OFF", "AUTO", "ON"][st.flash as usize]);
        icons::set_key(&self.wb_btn, icons::WB[st.wb], ["AUTO", "TUNG", "FLUO", "DAY", "CLDY"][st.wb.min(4)]);
        if st.wb > 0 {
            self.wb_btn.add_css_class("on");
        } else {
            self.wb_btn.remove_css_class("on");
        }
        if st.flash > 0 {
            self.flash_btn.add_css_class("on");
        } else {
            self.flash_btn.remove_css_class("on");
        }
        let b = BURSTS[st.burst];
        icons::set_key(&self.burst_btn, icons::BURST, &if b > 1 { format!("{b}X") } else { "OFF".to_string() });
        self.burst_badge.set_text(&format!("×{b}"));
        self.burst_badge.set_visible(b > 1);
        if b > 1 {
            self.burst_btn.add_css_class("on");
        } else {
            self.burst_btn.remove_css_class("on");
        }
        if st.grid > 0 {
            self.grid_btn.add_css_class("on");
        } else {
            self.grid_btn.remove_css_class("on");
        }
        icons::set_key(&self.assist_btn, if st.assist == 0 { icons::ASSIST_OFF } else { icons::ASSIST }, ["OFF", "PEAK", "ZEBRA", "BOTH"][st.assist as usize]);
        if st.assist > 0 {
            self.assist_btn.add_css_class("on");
        } else {
            self.assist_btn.remove_css_class("on");
        }
        self.view.set_assist(st.assist);
        icons::set_key(&self.afd_btn, icons::FOCUS_AUTO, if st.caf { "ON" } else { "OFF" });
        icons::set_key(&self.mode_btn, icons::MODES[st.mode.index()], ["AUTO", "ISO", "TIME", "MAN"][st.mode.index()]);
        icons::set_key(&self.meter_btn, icons::METER[st.metering as usize], ["CTR", "SPOT", "ALL"][st.metering as usize]);
        icons::set_key(&self.geo_btn, if st.geotag { icons::GEO } else { icons::GEO_OFF }, if st.geotag { "ON" } else { "OFF" });
        set_class(&self.geo_btn, "on", st.geotag);
        icons::set_key(&self.strip_btn, icons::STRIP, STRIP_FNS[st.strip_fn].0);
        set_class(&self.strip_btn, "on", st.strip_fn != 0);
        set_class(&self.enc_turn[2], "gone", st.mode == Mode::Manual);
        for (k, key) in self.keys.iter().enumerate() {
            let pin = st.pinned >> k & 1 == 1;
            set_class(key, "pinned", pin);
            set_class(key, "stowed", self.stowed.get() && !pin);
            key.set_can_target(!(self.stowed.get() && !pin));
        }
        if st.caf {
            self.afd_btn.add_css_class("on");
        } else {
            self.afd_btn.remove_css_class("on");
        }
        let high = match st.contrast {
            1 => true,
            2 => false,
            _ => self.bright.get(),
        };
        if high != contrast() {
            CONTRAST.store(high, Ordering::Relaxed);
            if let Some(w) = self.view.root() {
                set_class(&w, "contrast", high);
            }
            for c in self.encoders.iter().chain([&self.flyout, &self.shutter, &self.hist_area]) {
                c.queue_draw();
            }
            self.enc_shown.set([0; 3]);
        }
        if st.accent != ACCENT_IDX.load(Ordering::Relaxed) {
            ACCENT_IDX.store(st.accent, Ordering::Relaxed);
            self.css.load_from_string(&css(st.accent));
            for c in self.encoders.iter().chain([&self.flyout, &self.shutter, &self.hist_area, &self.marks]) {
                c.queue_draw();
            }
            self.focus_area.queue_draw();
            self.enc_shown.set([0; 3]);
        }
        let saved = st.saved();
        let busy = st.busy;
        let enc = [Dial::Iso, Dial::Shutter, Dial::Ev].map(|d| encoder_key(&st, d));
        for (k, d) in [Dial::Iso, Dial::Shutter, Dial::Ev].into_iter().enumerate() {
            let (_, value, frac, _) = encoder_shows(&st, d);
            self.enc_roll[k].set(&self.encoders[k], &value, frac);
        }
        let grid = st.grid | (st.histogram as u8) << 4;
        let geotag = st.geotag && !st.asleep;
        drop(st);
        // geotagging: the location client while it's on and the preview runs
        {
            let mut geo = self.geo.borrow_mut();
            if geotag != geo.running() {
                if geotag {
                    geo.start();
                } else {
                    geo.stop();
                }
            }
        }
        if *self.last_saved.borrow() != saved {
            settings::save(&saved);
            *self.last_saved.borrow_mut() = saved;
        }
        // the dials and the shutter button, when what they show changes (a redraw is a whole
        // Cairo surface made again and uploaded)
        if self.dials_shown.replace(Some(busy)) != Some(busy) {
            self.shutter.queue_draw();
        }
        // an encoder, when what it shows has changed
        let before = self.enc_shown.replace(enc);
        for (k, c) in self.encoders.iter().enumerate() {
            if before[k] != enc[k] {
                c.queue_draw();
            }
        }
        // the grid and the histogram: only when switched (the histogram's updates redraw it)
        if self.marks_grid.replace(grid) != grid {
            self.marks.set_visible(grid & 0xf != 0);
            self.marks.queue_draw();
            self.hist_area.set_visible(grid & 0x10 != 0);
            self.hist_area.queue_draw();
        }
    }

    // the old way of saying something: a failure or a warning is an alert, the rest a pill
    fn show_status(self: &Rc<Self>, msg: &str, secs: u64) {
        let warn = ["failed", "less than 1 GB", "no photo transfers", "no light-ccb", "pocket", "needs more room"]
            .iter()
            .any(|w| msg.contains(w));
        if warn {
            let icon = if msg.contains("pocket") { icons::MOON } else { icons::INFO };
            self.show_alert("status", icon, msg, "", secs.max(3));
        } else {
            self.show_pill(icons::STORAGE, msg, secs.max(2));
        }
    }

    // a small pill at the top: @icon and @title, for @secs
    fn show_pill(self: &Rc<Self>, icon: char, title: &str, secs: u64) {
        self.pill_label.set_markup(&icons::markup(icon, title));
        set_class(&self.pill_turn, "off", false);
        self.place_alert();
        let a = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_secs(secs), move || {
            *a.pill_timer.borrow_mut() = None;
            set_class(&a.pill_turn, "off", true);
            a.place_alert();
        });
        if let Some(old) = self.pill_timer.borrow_mut().replace(id) {
            old.remove();
        }
    }

    // an alert at the top, under the pill: @icon, @title and a line; for @secs, or until cleared
    // (@secs 0). @key says what it is, so only that can clear it
    fn show_alert(self: &Rc<Self>, key: &'static str, icon: char, title: &str, text: &str, secs: u64) {
        self.alert_icon.set_markup(&format!("<span font_family=\"{}\" size=\"220%\">{icon}</span>", icons::FAMILY));
        self.alert_title.set_text(title);
        self.alert_text.set_text(text);
        self.alert_text.set_visible(!text.is_empty());
        self.alert_key.set(key);
        self.lens_art.set_visible(key == "lens");
        self.alert_icon.set_visible(key != "lens");
        self.lens_art.queue_draw();
        set_class(&self.alert_turn, "off", false);
        self.place_alert();
        if let Some(old) = self.alert_timer.borrow_mut().take() {
            old.remove();
        }
        if secs > 0 {
            let a = self.clone();
            let id = glib::timeout_add_local_once(Duration::from_secs(secs), move || {
                *a.alert_timer.borrow_mut() = None;
                a.alert_key.set("");
                set_class(&a.alert_turn, "off", true);
            });
            *self.alert_timer.borrow_mut() = Some(id);
        }
    }

    // the camera's back, the covered lenses lit: where the lens is blocked
    fn draw_lens_art(&self, cr: &cairo::Context, w: f64, h: f64) {
        let mask = self.st.borrow().lens_mask;
        let (bw, bh) = (160.0, 93.0);
        let (x0, y0) = ((w - bw) / 2.0, (h - bh) / 2.0);
        let (r, cut) = (6.0, 22.0);
        cr.new_path();
        cr.arc(x0 + r, y0 + r, r, PI, 1.5 * PI);
        cr.line_to(x0 + bw - cut, y0);
        cr.line_to(x0 + bw, y0 + cut * 0.55);
        cr.arc(x0 + bw - r, y0 + bh - r, r, 0.0, 0.5 * PI);
        cr.arc(x0 + r, y0 + bh - r, r, 0.5 * PI, PI);
        cr.close_path();
        cr.set_source_rgb(0.2, 0.2, 0.22);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(0.95, 0.95, 0.95);
        cr.set_line_width(2.5);
        let _ = cr.stroke();
        let at = [
            (x0, y0 + 10.0),
            (x0, y0 + bh / 2.0),
            (x0, y0 + bh - 10.0),
            (x0 + bw / 2.0, y0),
            (x0 + bw / 2.0, y0 + bh),
        ];
        let (ar, ag, ab) = accent();
        for (i, (x, y)) in at.iter().enumerate() {
            if mask & (1 << i) == 0 {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.25);
                cr.arc(*x, *y, 4.0, 0.0, 2.0 * PI);
                let _ = cr.fill();
                continue;
            }
            for (rad, a) in [(16.0, 0.22), (11.0, 0.4)] {
                cr.set_source_rgba(ar, ag, ab, a);
                cr.arc(*x, *y, rad, 0.0, 2.0 * PI);
                let _ = cr.fill();
            }
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.arc(*x, *y, 6.0, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
    }

    // the alert away, if it is the @key one
    fn clear_alert(&self, key: &str) {
        if self.alert_key.get() != key {
            return;
        }
        self.alert_key.set("");
        set_class(&self.alert_turn, "off", true);
        if let Some(old) = self.alert_timer.borrow_mut().take() {
            old.remove();
        }
    }

    // the alert under the pill, or at the top without one
    fn place_alert(&self) {
        let pill = !self.pill_turn.has_css_class("off");
        place_on_edge(&self.alert_turn, self.quarter.get(), true, if pill { 64 } else { 10 });
    }

    // the key's icon now (for its pill)
    fn tool_icon(&self, t: Tool) -> char {
        let st = self.st.borrow();
        match t {
            Tool::Flash => [icons::FLASH_OFF, icons::FLASH_AUTO, icons::FLASH][st.flash as usize],
            Tool::Wb => icons::WB[st.wb.min(4)],
            Tool::Timer => if TIMERS[st.timer] == 0 { icons::TIMER_OFF } else { icons::TIMER },
            Tool::Grid => if st.grid == 0 { icons::GRID_OFF } else { icons::GRID },
            Tool::Histogram => icons::HISTOGRAM,
            Tool::Assist => if st.assist == 0 { icons::ASSIST_OFF } else { icons::ASSIST },
            Tool::Burst => icons::BURST,
            Tool::Afd => icons::FOCUS_AUTO,
            Tool::Meter => icons::METER[st.metering as usize],
            Tool::Geo => if st.geotag { icons::GEO } else { icons::GEO_OFF },
            Tool::Strip => icons::STRIP,
        }
    }

    // the room under the lens strip for the overheating warning, eased in and out
    fn animate_thermal(self: &Rc<Self>, to: f64) {
        if let Some(id) = self.thermal_anim.borrow_mut().take() {
            id.remove();
        }
        let a = self.clone();
        let last = Cell::new(0i64);
        let id = self.right.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let before = last.replace(now);
            let dt = if before == 0 { 0.016 } else { ((now - before) as f64 / 1e6).clamp(0.001, 0.05) };
            let cur = a.thermal_t.get();
            let step = dt / 0.3;
            let next = if to > cur { (cur + step).min(to) } else { (cur - step).max(to) };
            a.thermal_t.set(next);
            a.apply_deck();
            if next == to {
                a.thermal_anim.replace(None);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.thermal_anim.borrow_mut() = Some(id);
    }

    fn set_dial(&self, dial: Dial, pos: f64) {
        let before = {
            let st = self.st.borrow();
            match dial {
                Dial::Iso => st.iso,
                Dial::Shutter => st.shutter,
                Dial::Ev => st.ev,
            }
        };
        // 1/3 stops: the nearest list value (EV is always in thirds)
        let pos = if self.st.borrow().continuous || dial == Dial::Ev {
            pos
        } else {
            dial_ticks(dial)[dial_tick(dial, pos.clamp(0.0, 1.0))]
        };
        if dial_tick(dial, before) != dial_tick(dial, pos.clamp(0.0, 1.0)) {
            self.buzz(6);
        }
        {
            let mut st = self.st.borrow_mut();
            match dial {
                Dial::Iso => st.iso = pos.clamp(0.0, 1.0),
                Dial::Shutter => st.shutter = pos.clamp(0.0, 1.0),
                Dial::Ev => st.ev = pos.clamp(0.0, 1.0),
            }
        }
        // through the control thread: a focus run holds the driver for seconds
        let ctl = match dial {
            Dial::Iso => (ccb::ISO, iso_at(self.st.borrow().iso)),
            Dial::Shutter => (ccb::EXPOSURE_US, self.exposure_us()),
            Dial::Ev => (ccb::EV, ev_at(self.st.borrow().ev)),
        };
        let _ = self.ctl_tx.send(ctl);
        self.refresh();
        self.update_wheels();
    }

    // white balance: libcamera's AWB (auto), or a preset's gains for the preview module
    fn apply_wb(&self) {
        let Some(src) = self.pipeline.by_name("src") else { return };
        let (preset, module) = {
            let st = self.st.borrow();
            (st.wb, st.module)
        };
        let gains = self.cal.gains(preset, module);
        eprintln!("nebula: white balance {} (module {module}): {gains:?}", wb::PRESETS[preset]);
        match gains {
            None => src.set_property("awb-enable", true),
            Some((r, b)) => {
                src.set_property("colour-gains", gst::Array::new([r, b]));
                src.set_property("awb-enable", false);
            }
        }
    }

    fn set_mode(&self, mode: Mode) {
        self.st.borrow_mut().mode = mode;
        self.apply_exposure();
        self.refresh();
    }

    // a setting's choices, as (icon, name), and which is chosen
    fn choices(&self, o: Opt) -> (Vec<(char, String)>, usize) {
        let st = self.st.borrow();
        match o {
            Opt::Flash => (
                vec![(icons::FLASH_OFF, "off".into()), (icons::FLASH_AUTO, "auto".into()), (icons::FLASH, "on".into())],
                st.flash as usize,
            ),
            Opt::Wb => (
                wb::PRESETS.iter().zip(icons::WB).map(|(n, i)| (i, n.to_string())).collect(),
                st.wb,
            ),
            Opt::Timer => (
                TIMERS
                    .iter()
                    .map(|&t| if t == 0 { (icons::TIMER_OFF, "off".into()) } else { (icons::TIMER, format!("{t}s")) })
                    .collect(),
                st.timer,
            ),
            Opt::Grid => (
                vec![(icons::GRID_OFF, "off".into()), (icons::GRID, "3×3".into()), (icons::GRID, "golden".into())],
                st.grid as usize,
            ),
            Opt::Burst => (
                BURSTS.iter().map(|&b| (icons::BURST, if b > 1 { b.to_string() } else { "off".into() })).collect(),
                st.burst,
            ),
            Opt::Meter => (
                vec![
                    (icons::METER[0], "centre".into()),
                    (icons::METER[1], "touch".into()),
                    (icons::METER[2], "whole".into()),
                ],
                st.metering as usize,
            ),
            Opt::Assist => (
                vec![
                    (icons::ASSIST_OFF, "off".into()),
                    (icons::ASSIST, "peaking".into()),
                    (icons::ASSIST, "zebras".into()),
                    (icons::ASSIST, "both".into()),
                ],
                st.assist as usize,
            ),
        }
    }

    fn tool_button(&self, t: Tool) -> &gtk::Button {
        match t {
            Tool::Flash => &self.flash_btn,
            Tool::Wb => &self.wb_btn,
            Tool::Timer => &self.timer_btn,
            Tool::Grid => &self.grid_btn,
            Tool::Histogram => &self.hist_btn,
            Tool::Burst => &self.burst_btn,
            Tool::Assist => &self.assist_btn,
            Tool::Afd => &self.afd_btn,
            Tool::Meter => &self.meter_btn,
            Tool::Geo => &self.geo_btn,
            Tool::Strip => &self.strip_btn,
        }
    }

    // a key: the next of its choices, or a switch flipped; a bubble says what it did
    fn tool_tap(self: &Rc<Self>, t: Tool) {
        self.buzz(8);
        match t.opt() {
            Some(o) => {
                let (choices, now) = self.choices(o);
                self.choose(o, (now + 1) % choices.len());
            }
            None => match t {
                Tool::Strip => self.cycle_strip(),
                _ => {
                    {
                        let mut st = self.st.borrow_mut();
                        match t {
                            Tool::Histogram => st.histogram = !st.histogram,
                            Tool::Geo => st.geotag = !st.geotag,
                            _ => st.caf = !st.caf,
                        }
                    }
                    self.refresh();
                }
            },
        }
        let (title, _) = self.tool_note(t);
        self.show_pill(self.tool_icon(t), &title, 2);
    }

    // after a setting changes: the driver's side of it, the screen, the settings file
    fn setting_changed(&self) {
        let meter = self.st.borrow().metering;
        let _ = self.ctl_tx.send((ccb::METERING, meter as i32));
        self.refresh();
    }

    fn choose(&self, o: Opt, k: usize) {
        match o {
            Opt::Flash => {
                self.st.borrow_mut().flash = k as u8;
                let _ = self.ctl_tx.send((ccb::FLASH, k as i32));
            }
            Opt::Wb => {
                self.st.borrow_mut().wb = k;
                self.apply_wb();
            }
            Opt::Timer => self.st.borrow_mut().timer = k,
            Opt::Grid => self.st.borrow_mut().grid = k as u8,
            Opt::Burst => self.st.borrow_mut().burst = k,
            Opt::Assist => self.st.borrow_mut().assist = k as u8,
            Opt::Meter => {
                self.st.borrow_mut().metering = k as u8;
                let _ = self.ctl_tx.send((ccb::METERING, k as i32));
            }
        }
        self.refresh();
    }

    // portrait or landscape, from iio-sensor-proxy: the window's class turns the icons in
    // place ("spin"), the rotators re-lay their text out, the wheels turn their labels
    fn follow_orientation(&self, accel: &gtk::gio::DBusProxy) {
        let o = accel.cached_property("AccelerometerOrientation").and_then(|v| v.get::<String>());
        let q = o.as_deref().and_then(quarter_for);
        if q.is_some_and(|q| q != self.quarter.get()) {
            eprintln!("nebula: orientation {o:?} -> quarter {q:?} (was {})", self.quarter.get());
        }
        let Some(q) = q else { return };
        self.apply_quarter(q);
    }

    fn apply_quarter(&self, q: i32) {
        if self.quarter.replace(q) == q {
            return;
        }
        if let Some(w) = self.view.root() {
            w.remove_css_class("rot-cw");
            w.remove_css_class("rot-ccw");
            match q {
                1 => w.add_css_class("rot-cw"),
                -1 => w.add_css_class("rot-ccw"),
                _ => {}
            }
        }
        for r in &self.rotators {
            r.set_quarter(q);
        }
        self.place_status(q);
    }

    // the status line along the preview's top edge as the camera is held: the top, or the
    // left with the shutter down (turned -90), the right with it up; the overheating
    // warning along the opposite edge, so neither covers the middle of the frame
    fn place_status(&self, q: i32) {
        place_on_edge(&self.pill_turn, q, true, 10);
        self.place_alert();
    }

    // the driver restarts the ASICs' preview on the new module; the stream carries on
    // (it takes a moment: off the UI thread). The crop changes with the new module's
    // first frame, from switched().
    fn switch_module(&self, module: usize) {
        let (tx, rx) = mpsc::channel();
        self.st.borrow_mut().switching = Some(rx);
        thread::spawn(move || {
            if let Some(c) = ccb::Ccb::open() {
                c.set(ccb::MODULE, module as i32);
            }
            let _ = tx.send(module);
        });
    }

    fn switched(self: &Rc<Self>) {
        let done = {
            let st = self.st.borrow();
            st.switching.as_ref().and_then(|rx| rx.try_recv().ok())
        };
        let Some(module) = done else { return };
        // the old module's frames stop before the driver returns: the next frame is the
        // new module's
        let (zoom, want) = {
            let mut st = self.st.borrow_mut();
            st.switching = None;
            st.module = module;
            (st.zoom, preview_module(st.zoom))
        };
        self.view.set_zoom_next_frame(zoom / MODULE_MM[module]);
        self.apply_wb();
        if want != module {
            self.switch_module(want);
        }
    }

    // the zoom to @zoom exactly (a key, a step)
    fn set_zoom(self: &Rc<Self>, zoom: f64) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.st.borrow_mut().zoom_raw = zoom;
        self.apply_zoom(zoom);
    }

    // the zoom as a gesture takes it: a finger, the strip. It snaps to the prime focal lengths
    // (within 3.5 %), and stays there until the gesture has gone 7 % past
    fn zoom_gesture(self: &Rc<Self>, raw: f64) {
        let raw = raw.clamp(ZOOM_MIN, ZOOM_MAX);
        let held = {
            let mut st = self.st.borrow_mut();
            st.zoom_raw = raw;
            PRIMES.iter().copied().find(|&p| {
                let d = (raw / p).ln().abs();
                d < 0.035 || ((st.zoom - p).abs() < 0.01 && d < 0.07)
            })
        };
        self.apply_zoom(held.unwrap_or(raw));
    }

    fn apply_zoom(self: &Rc<Self>, zoom: f64) {
        let before = self.st.borrow().zoom;
        let (lo, hi) = (before.min(zoom), before.max(zoom));
        if PRIMES.iter().any(|&p| p > lo + 0.01 && p <= hi + 0.01 && (p - before).abs() > 0.01) {
            self.buzz(15);
        } else if zoom_dot(before) != zoom_dot(zoom) {
            self.buzz(5);
        }
        {
            let mut st = self.st.borrow_mut();
            st.zoom = zoom;
            st.zoom_wheel_until = Some(Instant::now() + Duration::from_millis(700));
            if let Some(id) = st.settle.take() {
                id.remove();
            }
            self.view.set_zoom(zoom / MODULE_MM[st.module]);
            // the ASICs follow the zoom as stock's app sends it, every 30-50 ms
            if st.zoom_sent.elapsed() >= Duration::from_millis(40) {
                st.zoom_sent = Instant::now();
                let _ = self.ctl_tx.send((ccb::ZOOM, (zoom / ZOOM_MIN * 1000.0).round() as i32));
            }
        }
        // once it settles: the last factor, the mirrors, and the preview module
        let app = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(200), move || {
            let (want, have, busy, zoom) = {
                let mut st = app.st.borrow_mut();
                st.settle = None;
                (preview_module(st.zoom), st.module, st.busy || st.switching.is_some(), st.zoom)
            };
            let _ = app.ctl_tx.send((ccb::ZOOM, (zoom / ZOOM_MIN * 1000.0).round() as i32));
            let _ = app.ctl_tx.send((ccb::MIRRORS, 1));
            if want != have && !busy {
                app.switch_module(want);
            }
        });
        self.st.borrow_mut().settle = Some(id);
        self.refresh();
        self.update_wheels();
        let a = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(750), move || a.update_wheels());
    }

    // a tap on the lens pill: the zoom eased there over about 130 ms (in steps of equal ratio)
    fn zoom_to(self: &Rc<Self>, target: f64) {
        let from = self.st.borrow().zoom;
        if (from - target).abs() < 0.01 {
            return;
        }
        let (steps, mut n) = (8, 0);
        let a = self.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            n += 1;
            let t = n as f64 / steps as f64;
            let eased = 1.0 - (1.0 - t).powi(2);
            a.set_zoom(from * (target / from).powf(eased));
            // (no zoom wheel over a tap: set_zoom shows it)
            a.st.borrow_mut().zoom_wheel_until = None;
            a.update_wheels();
            if n >= steps {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn step_prime(self: &Rc<Self>, up: bool) {
        let z = self.st.borrow().zoom;
        let next = if up {
            PRIMES.iter().copied().find(|&p| p > z + 0.5)
        } else {
            PRIMES.iter().rev().copied().find(|&p| p < z - 0.5)
        };
        if let Some(p) = next {
            self.set_zoom(p);
        }
    }

    // focus on @at (preview coordinates), or the centre: a 200x200 window in the module's
    // 4160x3120 pixels, of which the preview shows the middle 4096x3072 (the driver's
    // preview crop), through the zoom's crop (the driver runs AF in the background)
    fn focus(self: &Rc<Self>, at: Option<(f64, f64)>) {
        // AF-D leaves a focus by hand alone for a moment: stock's caf.disabled.post.tap is
        // 5 s (a setting there), which kept a whip pan right after a tap waiting too long;
        // the gyro's threshold already ignores the jolt of a tap and gentle reframing
        self.st.borrow_mut().caf_pause_until = Some(Instant::now() + Duration::from_millis(1500));
        self.focus_run(at, true);
    }

    // where a half press focuses: stock's "previous coordinate" (triggerAeFocusAtLastPoint),
    // the spot last tapped, until AF-D refocuses the centre (the camera moved, or zoomed:
    // focus_run(None) clears it) or the zoom has changed since (stock refocuses the centre
    // on a zoom, AF-D or not)
    fn last_focus_point(&self) -> Option<(f64, f64)> {
        let st = self.st.borrow();
        let same_zoom = st.caf_zoom.is_some_and(|z| (st.zoom - z).abs() <= 1.0);
        if same_zoom { st.focus_at } else { None }
    }

    // @marks: show the focus marks
    fn focus_run(self: &Rc<Self>, at: Option<(f64, f64)>, marks: bool) {
        if self.st.borrow().busy || self.focusing.swap(true, Ordering::SeqCst) {
            return;
        }
        // AF-D's zoom trigger counts from here
        let zoom = self.st.borrow().zoom;
        self.st.borrow_mut().caf_zoom = Some(zoom);
        let (w, h) = (self.view.width() as f64, self.view.height() as f64);
        let z = self.view.zoom();
        let (px, py) = at.unwrap_or((w / 2.0, h / 2.0));
        let sx = 2080.0 + (px - w / 2.0) / w * 4096.0 / z;
        let sy = 1560.0 + (py - h / 2.0) / h * 3072.0 / z;
        let fx = ((sx - 100.0).round() as i32).clamp(0, 4160 - 200);
        let fy = ((sy - 100.0).round() as i32).clamp(0, 3120 - 200);
        if marks {
            let mut st = self.st.borrow_mut();
            st.focus_until = Some(Instant::now() + Duration::from_secs(12));
            st.focus_at = at;
            st.focus_t0 = Some(Instant::now());
            st.focus_done = None;
        }
        if marks {
            self.place_focus(at);
        }
        // the run's outcome, from the driver once the ASICs answer (seconds on B and C)
        let (focusing, outcome) = (self.focusing.clone(), self.af_outcome.clone());
        outcome.store(0, Ordering::SeqCst);
        thread::spawn(move || {
            let mut result = 2;
            if let Some(c) = ccb::Ccb::open() {
                c.set(ccb::FOCUS_X, fx);
                c.set(ccb::FOCUS_Y, fy);
                c.set(ccb::AF_START, 1);
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_secs(7) {
                    thread::sleep(Duration::from_millis(50));
                    match c.get(ccb::AF_RESULT) {
                        Some(r @ (1 | 2)) => {
                            result = r;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            outcome.store(result, Ordering::SeqCst);
            focusing.store(false, Ordering::SeqCst);
        });
        if marks && !self.marks_ticking.replace(true) {
            // animate the marks until they go (one callback however many runs)
            let a = self.clone();
            self.focus_area.add_tick_callback(move |m, _| {
                a.focus_outcome();
                m.queue_draw();
                if a.st.borrow().focus_until.is_some_and(|t| Instant::now() < t) {
                    glib::ControlFlow::Continue
                } else {
                    a.marks_ticking.set(false);
                    m.set_visible(false);
                    glib::ControlFlow::Break
                }
            });
        }
    }

    // the focus layer over the focus point (@at, or the preview's middle), kept on the preview
    fn place_focus(&self, at: Option<(f64, f64)>) {
        let (w, h) = (self.view.width() as f64, self.view.height() as f64);
        let (cx, cy) = at.unwrap_or((w / 2.0, h / 2.0));
        let (fw, fh) = FOCUS_AREA;
        let x = (cx - fw / 2.0).clamp(0.0, (w - fw).max(0.0));
        let y = (cy - fh * 0.55).clamp(0.0, (h - fh).max(0.0));
        self.focus_area.set_margin_start(x as i32);
        self.focus_area.set_margin_top(y as i32);
        self.focus_centre.set((cx - x, cy - y));
        self.focus_area.set_visible(true);
        self.focus_area.queue_draw();
    }

    // the focus run's outcome, once known: the marks' end (stock's CrossHair: 5 s after it)
    fn focus_outcome(&self) {
        let r = self.af_outcome.load(Ordering::SeqCst);
        let mut st = self.st.borrow_mut();
        if r != 0 && st.focus_t0.is_some() && st.focus_done.is_none() {
            let now = Instant::now();
            st.focus_done = Some((r, now));
            st.focus_until = Some(now + Duration::from_secs(5));
            drop(st);
            if r == 1 {
                self.buzz(10);
            }
        }
    }

    fn shutter_pressed(self: &Rc<Self>) {
        self.close_settings();
        let (busy, counting, t) = {
            let st = self.st.borrow();
            (st.busy, st.counting, TIMERS[st.timer])
        };
        if busy || counting || self.st.borrow().battery_low || self.st.borrow().thermal == 2 || !self.room_for(BURSTS[self.st.borrow().burst]) {
            return;
        }
        // stock's: no photo with less than 1 GB free
        if photos_free().is_some_and(|f| f < 1 << 30) {
            self.show_status("less than 1 GB free: make some space to take photos", 4);
            return;
        }
        if t == 0 {
            return self.capture();
        }
        self.st.borrow_mut().counting = true;
        let left = Rc::new(RefCell::new(t));
        self.countdown.set_text(&t.to_string());
        self.countdown.set_visible(true);
        let app = self.clone();
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let mut n = left.borrow_mut();
            *n -= 1;
            if *n == 0 {
                app.countdown.set_visible(false);
                app.st.borrow_mut().counting = false;
                app.capture();
                return glib::ControlFlow::Break;
            }
            app.countdown.set_text(&n.to_string());
            glib::ControlFlow::Continue
        });
    }

    // As stock: the driver takes the photo while the preview runs (it pauses for the
    // exposure), with the preview's exposure and focus, and the next one can be taken right
    // away. The records come over the CSI links beside the preview, photo after photo, and
    // are joined into LRIs in the background (a burst: one per frame).
    // room in /tmp for a photo of @frames frames on its way (about 300 MB each: up to 17
    // records of 16 MB), with a margin for the LRI assembly
    fn room_for(self: &Rc<Self>, frames: u8) -> bool {
        let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
        let path = std::ffi::CString::new("/tmp").unwrap();
        if unsafe { libc::statvfs(path.as_ptr(), &mut fs) } != 0 {
            return true;
        }
        let free = fs.f_bavail as u64 * fs.f_frsize as u64;
        let need = (frames as u64 + 1) * 300 << 20;
        let room = free > need;
        if !room {
            // more than the whole of /tmp: it will never fit, whatever is saving
            let total = fs.f_blocks as u64 * fs.f_frsize as u64;
            if need > total {
                self.show_status(&format!("a burst of {frames} needs more room than there is: choose a smaller burst"), 4);
            } else {
                self.show_status("waiting for photos to save", 2);
            }
        }
        room
    }

    fn capture(self: &Rc<Self>) {
        self.hold_sleep();
        self.feedback("camera-shutter");
        self.flash_shutter();
        if self.st.borrow().sparkle {
            led::sparkle(accent());
        }
        let (zoom, burst, seq, dark, stacked) = {
            let mut st = self.st.borrow_mut();
            st.busy = true;
            st.saving += 1;
            st.seq += 1;
            (st.zoom, BURSTS[st.burst], st.seq, st.mode == Mode::Auto && st.live_iso > 400, st.stacked)
        };
        self.thumb.set_paintable(self.preview_still(88.0, 66.0).as_ref());
        if burst > 1 {
            self.start_burst_screen(burst);
        } else {
            self.blackout.set_opacity(1.0);
            self.blackout.set_visible(true);
        }
        self.thumb.set_opacity(0.5);
        let app = self.clone();
        self.thumb_spin.add_tick_callback(move |w, _| {
            w.queue_draw();
            if app.st.borrow().saving > 0 {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        self.refresh();

        // the modules for the zoom, as stock's 28/70/150 sets (bit n + 1 = LRI camera n)
        let cams = match module_for(zoom) {
            0 => 0..10,  // A1-A5 B1-B5
            1 => 5..16,  // B1-B5 C1-C6
            _ => 10..16, // C1-C6
        };
        let mask = cams.fold(0u32, |m, i| m | 1 << (i + 1));
        let stamp = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%Y%m%d_%H%M%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "photo".into());
        let dir = PathBuf::from(format!("/tmp/l16-shot-{stamp}-{seq}"));
        let view = {
            let st = self.st.borrow();
            let iso = if st.mode.fixes_iso() { iso_at(st.iso) } else { st.live_iso };
            let secs = if st.mode.fixes_shutter() { secs_at(st.shutter) } else { st.live_secs };
            let mut v = vec![
                "--iso".to_string(),
                iso.to_string(),
                "--exposure-us".into(),
                ((secs * 1e6).round() as u64).to_string(),
                "--awb-mode".into(),
                wb::AWB_MODE[st.wb].to_string(),
            ];
            if let Some((r, b)) = self.cal.gains(st.wb, st.module) {
                v.push("--wb".into());
                v.push(format!("{r},{b}"));
            }
            match self.quarter.get() {
                -1 => v.extend(["--orientation".into(), "1".into()]),
                1 => v.extend(["--orientation".into(), "2".into()]),
                _ => {}
            }
            if let Some(f) = st.geotag.then(|| self.geo.borrow().fix()).flatten() {
                let alt = f.altitude.map_or("nan".to_string(), |a| a.to_string());
                v.push("--gps".into());
                v.push(format!("{},{},{},{alt},{}", f.lat, f.lon, f.accuracy, f.unix_secs));
            }
            v
        };
        self.photo_args.borrow_mut().insert(dir.clone(), view);
        let tx = self.stage_tx.clone();
        let turn = self.transfer_turn.clone();
        let Some(queue) = self.transfers.borrow().as_ref().map(|t| t.queue.clone()) else {
            self.st.borrow_mut().busy = false;
            self.fade_blackout();
            self.saved();
            return self.show_status("capture failed: no transfer streams", 6);
        };
        // Quick shots: no precapture metering (about half a second; the preview is metered)
        // and one frame per module. In the dark, as stock: the precapture metering, and the
        // ASICs stack several exposures per module when they judge it needed (slower, 4x
        // the data, less noise; the settings can turn stacking off). Bursts are always quick.
        let flags = if dark && burst == 1 {
            if stacked {
                0
            } else {
                ccb::CAPTURE_NO_STACK
            }
        } else {
            ccb::CAPTURE_NO_PRECAPTURE | ccb::CAPTURE_NO_STACK
        };
        thread::spawn(move || {
            let c = match ccb::Ccb::open() {
                Some(c) => c,
                None => return drop(tx.send(Stage::Captured(Err("no camera driver".into())))),
            };
            let t = Instant::now();
            let cap = match c.capture(mask, burst, flags) {
                Ok(cap) => cap,
                Err(e) => return drop(tx.send(Stage::Captured(Err(e)))),
            };
            eprintln!(
                "nebula: captured {} in {:.2} s: records {:?} (burst {burst}, status {})",
                dir.display(),
                t.elapsed().as_secs_f64(),
                cap.records,
                cap.status
            );
            // the records come in the order asked for: photos take turns
            let _turn = turn.lock().unwrap();
            match transfer::Photo::new(dir.clone(), cap.records) {
                Ok(p) => queue.lock().unwrap().push_back(p),
                Err(e) => return drop(tx.send(Stage::Captured(Err(e.to_string())))),
            }
            let _ = tx.send(Stage::Captured(Ok(dir.clone())));
            // ASIC by ASIC; at most four in flight per ASIC (a stream has eight buffers)
            let got = |a: usize| -> Option<u16> {
                let q = queue.lock().unwrap();
                q.iter().find(|p| p.dir == dir).map(|p| p.received(a))
            };
            let t = Instant::now();
            let mut err = None;
            'asics: for a in 0..3 {
                for k in 0..cap.records[a] {
                    let start = Instant::now();
                    while got(a).is_some_and(|n| n + 4 <= k) {
                        if start.elapsed() > Duration::from_secs(3) {
                            err = Some(format!("ASIC{} record {k} did not arrive", a + 1));
                            break 'asics;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    if let Err(e) = c.transfer(a as u32) {
                        err = Some(e);
                        break 'asics;
                    }
                }
            }
            // the last records arrive within a moment; give up on missing ones
            for _ in 0..50 {
                if err.is_some() || got(0).is_none() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let mut q = queue.lock().unwrap();
            if let Some(i) = q.iter().position(|p| p.dir == dir) {
                q.remove(i);
                let _ = tx.send(Stage::Transferred(Err((dir.clone(), err.unwrap_or("records missing".into())))));
            } else {
                eprintln!("nebula: transferred {} in {:.2} s", dir.display(), t.elapsed().as_secs_f64());
            }
        });
    }

    // OpenLight's burst screen: the number counts up every exposure (100 ms at least), a
    // timer as stock's, then "saving captures" until the photo is taken
    fn start_burst_screen(self: &Rc<Self>, total: u8) {
        let step = {
            let mut st = self.st.borrow_mut();
            st.burst_count = 1;
            st.burst_captured = false;
            let secs = if st.mode.fixes_shutter() { secs_at(st.shutter) } else { st.live_secs };
            Duration::from_secs_f64(secs.max(0.1))
        };
        self.burst_label.set_text("1");
        self.burst_label.set_visible(true);
        self.burst_saving.set_visible(false);
        self.burst_screen.set_visible(true);
        let app = self.clone();
        glib::timeout_add_local(step, move || {
            let (n, captured) = {
                let mut st = app.st.borrow_mut();
                st.burst_count += 1;
                (st.burst_count, st.burst_captured)
            };
            if n <= total {
                app.burst_label.set_text(&n.to_string());
                return glib::ControlFlow::Continue;
            }
            if captured {
                app.end_burst_screen();
            } else {
                app.burst_label.set_visible(false);
            }
            glib::ControlFlow::Break
        });
    }

    fn burst_taken(&self) {
        let counting = {
            let mut st = self.st.borrow_mut();
            st.burst_captured = true;
            st.burst_count
        };
        // the counter still running shows its last numbers first
        if counting > 0 && !self.burst_label.is_visible() {
            self.end_burst_screen();
        }
    }

    fn end_burst_screen(&self) {
        self.st.borrow_mut().burst_count = 0;
        self.burst_screen.set_visible(false);
    }

    // the blackout fades as the preview returns (OpenLight: alpha 1 to 0)
    fn fade_blackout(&self) {
        let b = self.blackout.clone();
        let start = Instant::now();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            let t = start.elapsed().as_secs_f64() / 0.25;
            if t >= 1.0 {
                b.set_visible(false);
                return glib::ControlFlow::Break;
            }
            b.set_opacity(1.0 - t);
            glib::ControlFlow::Continue
        });
    }

    // No suspend while photos are on their way: the ASICs lose them when powered off, and
    // the phone suspends seconds after the screen blanks. A logind block inhibitor, held
    // until the last photo is saved.
    fn hold_sleep(&self) {
        if self.sleep_inhibitor.borrow().is_some() {
            return;
        }
        let r = gtk::gio::bus_get_sync(gtk::gio::BusType::System, None::<&gtk::gio::Cancellable>)
            .and_then(|bus| {
                bus.call_with_unix_fd_list_sync(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                    "Inhibit",
                    Some(&("sleep", "Camera", "Saving photos", "block").to_variant()),
                    Some(glib::VariantTy::new("(h)").unwrap()),
                    gtk::gio::DBusCallFlags::NONE,
                    -1,
                    None::<&gtk::gio::UnixFDList>,
                    None::<&gtk::gio::Cancellable>,
                )
            });
        match r {
            Ok((_, Some(fds))) => match fds.get(0) {
                Ok(fd) => *self.sleep_inhibitor.borrow_mut() = Some(fd),
                Err(e) => eprintln!("nebula: sleep inhibitor: {e}"),
            },
            Ok((_, None)) => eprintln!("nebula: sleep inhibitor: no fd"),
            Err(e) => eprintln!("nebula: sleep inhibitor: {e}"),
        }
    }

    // a feedbackd event (camera-shutter, camera-focus): the sound and vibration Phosh's
    // feedback profile gives it, or none in silent mode
    // a pulse of the vibration motor, at the haptics setting's strength
    fn buzz(&self, ms: u16) {
        let strength = [0, 30000, 60000][self.st.borrow().haptics as usize];
        self.motor.play(ms, strength);
    }

    fn feedback(&self, event: &str) {
        let Ok(bus) = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, None::<&gtk::gio::Cancellable>)
        else {
            return;
        };
        let hints = glib::VariantDict::new(None).end();
        bus.call(
            Some("org.sigxcpu.Feedback"),
            "/org/sigxcpu/Feedback",
            "org.sigxcpu.Feedback",
            "TriggerEvent",
            Some(&("org.l16linux.Nebula", event, hints, -1i32).to_variant()),
            None,
            gtk::gio::DBusCallFlags::NONE,
            -1,
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    }

    fn saved(&self) {
        let left = {
            let mut st = self.st.borrow_mut();
            st.saving = st.saving.saturating_sub(1);
            st.saving
        };
        if left == 0 {
            self.thumb.set_opacity(1.0);
            // the photos are safe: the camera may sleep again
            self.sleep_inhibitor.borrow_mut().take();
        }
        self.thumb_spin.queue_draw();
    }

    // three dots going round over the thumbnail while photos are saved
    fn draw_thumb_spin(&self, cr: &cairo::Context, w: f64, h: f64) {
        if self.st.borrow().saving == 0 {
            return;
        }
        let t = glib::monotonic_time() as f64 / 1e6;
        for i in 0..3 {
            let a = t * 4.0 + i as f64 * 2.0 * PI / 3.0;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.arc(w / 2.0 + 10.0 * a.cos(), h / 2.0 + 10.0 * a.sin(), 3.0, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
    }

    fn run(cmd: &mut Command) -> Result<(), String> {
        match cmd.output() {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let out = String::from_utf8_lossy(&o.stdout);
                let last = err.lines().chain(out.lines()).filter(|l| !l.is_empty()).last();
                Err(last.unwrap_or("failed").to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn on_stage(self: &Rc<Self>, stage: Stage) {
        match stage {
            Stage::Captured(Ok(_)) => {
                self.st.borrow_mut().busy = false;
                self.fade_blackout();
                self.burst_taken();
            }
            Stage::Transferred(r) => {
                match r {
                    Ok(dir) => {
                        let out = glib::user_special_dir(glib::UserDirectory::Pictures)
                            .unwrap_or_else(|| glib::home_dir().join("Pictures"))
                            .join("L16");
                        let _ = std::fs::create_dir_all(&out);
                        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                        let stamp = name.unwrap_or_default().replace("l16-shot-", "");
                        let out = out.join(format!("L16_{stamp}.lri"));
                        let tx = self.stage_tx.clone();
                        let view = self.photo_args.borrow_mut().remove(&dir).unwrap_or_default();
                        thread::spawn(move || {
                            let mut raws: Vec<PathBuf> = (1..=3)
                                .map(|a| dir.join(format!("asic{a}.raw")))
                                .filter(|p| p.exists())
                                .collect();
                            raws.sort();
                            let r = App::run(Command::new("l16-lri-assemble").args(&view).arg(&out).args(&raws));
                            let _ = std::fs::remove_dir_all(&dir);
                            let _ = tx.send(Stage::Saved(r.map(|_| out)));
                        });
                    }
                    Err((dir, e)) => {
                        // its records (up to ~300 MB, in /tmp's RAM) go too, or the space
                        // check refuses every later photo
                        self.photo_args.borrow_mut().remove(&dir);
                        thread::spawn(move || {
                            let _ = std::fs::remove_dir_all(&dir);
                        });
                        self.saved();
                        self.show_status(&format!("capture failed: {e}"), 6);
                    }
                }
            }
            Stage::Captured(Err(e)) => {
                self.st.borrow_mut().busy = false;
                self.fade_blackout();
                self.burst_taken();
                self.saved();
                self.show_status(&format!("capture failed: {e}"), 6);
            }
            Stage::Saved(Ok(_)) => {
                self.saved();
                self.update_storage(true);
            }
            Stage::Saved(Err(e)) => {
                self.saved();
                self.show_status(&format!("saving failed: {e}"), 6);
            }
        }
        self.refresh();
    }

    // stock's device status: the battery (its icon steps at 90, 60, 35 and 15%) and, at 10% or
    // less, the battery-low screen over the camera until 12% (stock's hysteresis)
    fn update_battery(self: &Rc<Self>) {
        let read = |f: &str| std::fs::read_to_string(format!("/sys/class/power_supply/qcom-battery/{f}"));
        let Ok(level) = read("capacity").map(|s| s.trim().parse::<u8>().unwrap_or(100)) else { return };
        let charging = read("status").is_ok_and(|s| s.trim() == "Charging" || s.trim() == "Full");
        let (low, show) = {
            let st = self.st.borrow();
            (st.battery_low, st.device_status)
        };
        let low_now = if low { level < 12 } else { level <= 10 };
        if low_now && !low {
            self.buzz(60);
        }
        {
            let mut st = self.st.borrow_mut();
            st.battery = (level, charging);
            st.battery_low = low_now;
        }
        self.battery_screen.set_visible(low_now);
        let step = [90, 60, 35, 15].iter().position(|&t| level >= t).unwrap_or(4);
        let icon = if charging { icons::BATTERY_CHARGING[step] } else { icons::BATTERY[step] };
        self.battery_label.set_markup(&icons::markup(icon, &format!("{level}%")));
        self.status_box.set_visible(show);
    }

    // captures left: free space less stock's 500 MiB reserve, over the size of the last
    // photos (stacked ones are 4x a single one; stock's 180 MiB until there are some). With
    // @warn: stock's banner once the count runs low (under 25 here: stock's 200 is for its
    // 256 GB), and again at 5% left
    fn update_storage(self: &Rc<Self>, warn: bool) {
        let Some((free, total)) = photos_space() else { return };
        let reserve = 500u64 << 20;
        let free = free.saturating_sub(reserve);
        let pct = free as f64 / total.saturating_sub(reserve).max(1) as f64 * 100.0;
        let left = free / photo_size();
        self.st.borrow_mut().captures_left = left;
        self.storage_label.set_markup(&icons::markup(icons::STORAGE, &left.to_string()));
        if !warn {
            return;
        }
        let warned = self.st.borrow().storage_warned;
        if left >= 25 {
            self.st.borrow_mut().storage_warned = 0;
        } else if warned == 0 {
            self.st.borrow_mut().storage_warned = 1;
            self.show_status(&format!("{left} captures left"), 5);
        } else if pct <= 5.0 && warned == 1 {
            self.st.borrow_mut().storage_warned = 2;
            self.show_status(&format!("{pct:.1}% storage left"), 5);
        }
    }

    // stock's thermal levels (thermal-engine's LIGHT-CAMERA-TSENS on ASIC1's telemetry: 55
    // and 65 C, clear under 45 and 56), from the driver's hwmon "light_ccb" (no reading
    // while the preview is stopped, or for its first ~10 s). Hot: the cool-off screen, the
    // preview stopped for 2 minutes at a time until a new reading is under 56 C
    fn update_thermal(self: &Rc<Self>) {
        let temp = camera_temp();
        let (level, pause) = {
            let st = self.st.borrow();
            (st.thermal, st.thermal_pause_until)
        };
        let level = match (level, temp) {
            (0, Some(t)) if t >= 65 => 2,
            (0, Some(t)) if t >= 55 => 1,
            (1, Some(t)) if t >= 65 => 2,
            (1, Some(t)) if t < 45 => 0,
            (2, Some(t)) if t < 56 => 1,
            (l, _) => l,
        };
        let pause = if level == 2 && !pause.is_some_and(|p| Instant::now() < p) && temp.is_some_and(|t| t >= 56) {
            eprintln!("nebula: camera modules at {} C: cooling off", temp.unwrap_or(0));
            Some(Instant::now() + Duration::from_secs(120))
        } else if level == 2 {
            pause
        } else {
            None
        };
        {
            let mut st = self.st.borrow_mut();
            st.thermal = level;
            st.thermal_pause_until = pause;
        }
        set_class(&self.thermal_turn, "off", level != 1);
        self.animate_thermal(if level == 1 { 1.0 } else { 0.0 });
        self.hot_screen.set_visible(level == 2);
        self.follow_screen();
    }

    // a lens covered: stock's warning and its buzz (every pass of the fast loop)
    fn lens_check(self: &Rc<Self>) {
        let mask = self.blocked.load(Ordering::Relaxed);
        let (shown, warn) = (self.st.borrow().lens_mask, self.st.borrow().lens_warning);
        let mask = if warn > 0 { mask } else { 0 };
        if mask != shown {
            self.st.borrow_mut().lens_mask = mask;
            self.lens_art.queue_draw();
            if mask != 0 {
                self.show_alert("lens", icons::CAMERA, "Lens blocked", "Something is over the lit lens: move it away to see the whole frame.", 0);
            } else {
                self.clear_alert("lens");
            }
            if shown == 0 && warn == 2 {
                self.buzz(30);
            }
        }
    }

    fn poll(self: &Rc<Self>) {
        let n = {
            let mut st = self.st.borrow_mut();
            st.polls = st.polls.wrapping_add(1);
            st.polls
        };
        if n % 10 == 1 {
            self.update_battery();
            self.update_thermal();
        }
        if n % 100 == 1 {
            self.update_storage(n == 1);
        }
        // the metered exposure, which the driver mirrors into its controls
        if self.st.borrow().mode != Mode::Manual && !self.st.borrow().busy {
            let iso = self.metered[0].load(Ordering::Relaxed);
            let us = self.metered[1].load(Ordering::Relaxed);
            let mut st = self.st.borrow_mut();
            st.live_iso = iso;
            st.live_secs = us as f64 / 1e6;
        }
        self.continuous_focus();
        self.follow_screen();
        let still = self.still.load(Ordering::Relaxed);
        if still != self.st.borrow().tripod {
            self.st.borrow_mut().tripod = still;
            let _ = self.ctl_tx.send((ccb::TRIPOD, still as i32));
            self.set_badge("tripod", &self.tripod_badge, still, icons::CAMERA_LOCK, "Tripod mode", "The camera is still, so automatic photos may use a longer exposure.");
        }
        // stock's in-pocket check (BasePreviewFragment): two or more lenses covered and under
        // 2 lux for 30 s: say so and close
        let lux = self
            .light
            .as_ref()
            .and_then(|l| l.cached_property("LightLevel"))
            .and_then(|v| v.get::<f64>())
            .unwrap_or(f64::MAX);
        // high contrast in bright light: on above 5000 lux, off again below 3000
        if lux.is_finite() && lux < f64::MAX {
            let now = self.bright.get();
            if (!now && lux > 5000.0) || (now && lux < 3000.0) {
                self.bright.set(!now);
                self.refresh();
            }
        }
        let pocketed = self.blocked.load(Ordering::Relaxed).count_ones() >= 2 && lux < 2.0;
        let since = {
            let mut st = self.st.borrow_mut();
            st.pocket_since = if pocketed && st.pocket { Some(st.pocket_since.unwrap_or_else(Instant::now)) } else { None };
            st.pocket_since
        };
        // Unlike stock (which closes the app at 30 s with a two-second notice), a countdown from
        // 20 s, then the screen blanks as the power button blanks it: the device suspends
        // (light-lfc-suspend-on-blank) and the camera is there again on wake
        let held = since.map_or(0, |t| t.elapsed().as_secs());
        if held >= 30 {
            self.st.borrow_mut().pocket_since = None;
            eprintln!(
                "nebula: in a pocket (lenses covered {:#04b}, {lux:.1} lux, 30 s): blanking the screen",
                self.blocked.load(Ordering::Relaxed)
            );
            self.clear_alert("status");
            blank_screen();
        } else if held >= 20 {
            self.show_status(&format!("In a pocket? Sleeping in {} s", 30 - held), 2);
        }
        // stock's hand-shake assist: the photo's exposure longer than 1/70 s (1/150 s on the
        // 70 and 150 mm modules); not while tripod mode is on
        let shake = {
            let st = self.st.borrow();
            let secs = if st.mode.fixes_shutter() { secs_at(st.shutter) } else { st.live_secs };
            let limit = if st.zoom >= 70.0 { 0.006_67 } else { 0.014_36 };
            secs > limit && !st.tripod
        };
        self.set_badge("shake", &self.shake_badge, shake, icons::HAND_WAVE, "Hold steady", "The shutter is slow enough that a shaky hand will blur the photo. Brace the camera, or use a tripod.");
        // the moon: a stacked capture ahead (only where stacking is on: auto, the setting)
        let stacking = self.st.borrow().stacked && self.st.borrow().mode == Mode::Auto;
        self.set_badge("moon", &self.moon_badge, stacking && self.metered[3].load(Ordering::Relaxed) == 1, icons::MOON, "Stacked photo ahead", "It is dark: several exposures will be taken and combined. Hold still.");
        let (show, asleep) = {
            let st = self.st.borrow();
            (st.histogram, st.asleep)
        };
        if show && !asleep {
            self.update_histogram();
            self.hist_area.queue_draw();
        }
        self.refresh();
    }

    // the transfer streams once the preview is streaming (its first frame): setup links
    // ASIC1's path off the preview's CSID, which the preview's start would refuse (EPIPE).
    // The pipeline's start is asynchronous, so not straight after it
    fn start_transfers_on_frame(self: &Rc<Self>) {
        if self.transfers_wait.borrow().is_some() {
            return;
        }
        let a = self.clone();
        let id = self.paintable.connect_invalidate_contents(move |p| {
            if let Some(id) = a.transfers_wait.borrow_mut().take() {
                p.disconnect(id);
            }
            eprintln!("nebula: first preview frame");
            if !a.st.borrow().asleep && a.transfers.borrow().is_none() {
                a.start_transfers();
            }
        });
        *self.transfers_wait.borrow_mut() = Some(id);
    }

    // the photo transfer streams, beside the preview (done: Stage::Transferred); started after it
    fn start_transfers(self: &Rc<Self>) {
        let (done_tx, done_rx) = mpsc::channel();
        match transfer::Transfers::start(done_tx) {
            Ok(t) => *self.transfers.borrow_mut() = Some(t),
            Err(e) => {
                self.show_status(&format!("no photo transfers: {e}"), 8);
            }
        }
        let tx = self.stage_tx.clone();
        thread::spawn(move || {
            while let Ok(r) = done_rx.recv() {
                if tx.send(Stage::Transferred(r)).is_err() {
                    break;
                }
            }
        });
    }

    // The preview (and the ASICs, which the driver powers down a while after) stops while it
    // can't be seen: the screen off, another app in front (or the notification drawer; after a
    // second, so a glance away doesn't stop it), or the settings screen over it. It starts
    // again when it can. Not while a photo is on its way:
    // the ASICs hold it until it is transferred. As when the app closes and opens: the
    // transfer streams stop first and start after the preview (the preview cannot restart
    // under them).
    // the buttons, the touch strip and the photos' progress, every 15 ms (stopped while the
    // screen is off, so the app leaves the CPU alone; the buttons still count while the
    // preview is stopped behind another app or the settings)
    fn fast_loop(self: &Rc<Self>) {
        if std::mem::replace(&mut self.st.borrow_mut().fast_loop_on, true) {
            return;
        }
        let a = self.clone();
        glib::timeout_add_local(Duration::from_millis(15), move || {
            if a.st.borrow().asleep && a.st.borrow().screen_off {
                a.st.borrow_mut().fast_loop_on = false;
                return glib::ControlFlow::Break;
            }
            while let Ok(ev) = a.input_rx.try_recv() {
                a.on_input(ev);
            }
            a.switched();
            a.lens_check();
            while let Ok(stage) = a.stage_rx.try_recv() {
                a.on_stage(stage);
            }
            glib::ControlFlow::Continue
        });
    }

    fn follow_screen(self: &Rc<Self>) {
        // nothing to sleep or wake before the camera is this app's (take_camera)
        if !CAMERA_TAKEN.load(Ordering::Relaxed) {
            return;
        }
        let screen = std::fs::read_to_string("/sys/class/drm/card0-DSI-1/dpms")
            .map_or(true, |s| s.trim() == "On");
        let front = self.view.root().and_downcast::<gtk::Window>().map_or(true, |w| w.is_active());
        self.hold_front(front);
        let seen = front && !self.settings_page.is_visible();
        let unseen_since = {
            let mut st = self.st.borrow_mut();
            if seen {
                st.unseen_since = None;
            } else if st.unseen_since.is_none() {
                st.unseen_since = Some(Instant::now());
            }
            st.unseen_since
        };
        let cooling = self.st.borrow().thermal_pause_until.is_some_and(|t| Instant::now() < t);
        let away = unseen_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
            || (!seen && self.settings_page.is_visible())
            || cooling;
        let on = screen && !away;
        let was_off = std::mem::replace(&mut self.st.borrow_mut().screen_off, !screen);
        let (asleep, busy) = {
            let st = self.st.borrow();
            (st.asleep, st.busy || st.saving > 0 || st.counting)
        };
        if !on && !asleep && !busy {
            eprintln!("nebula: sleep (screen {screen}, away {away}): stopping");
            self.st.borrow_mut().asleep = true;
            self.gyro_on.store(false, Ordering::Relaxed);
            if let Some(mut t) = self.transfers.borrow_mut().take() {
                t.stop();
            }
            self.stop_preview();
            eprintln!("nebula: sleep: preview stopped");
        } else if on && asleep {
            eprintln!("nebula: wake (screen {screen}, front {front}, seen {seen}): starting the preview");
            self.st.borrow_mut().asleep = false;
            self.gyro_on.store(true, Ordering::Relaxed);
            let r = self.pipeline.set_state(gst::State::Playing);
            eprintln!("nebula: wake: set_state(Playing) = {r:?}");
            self.apply_exposure();
            self.apply_wb();
            self.start_transfers_on_frame();
            // presses while the screen was off are not for the camera
            if was_off {
                while self.input_rx.try_recv().is_ok() {}
            }
            self.fast_loop();
        }
    }

    // In front, as stock's camera: the screen stays on (only the in-pocket check or the power
    // button turns it off), and the display stays landscape even with the rotation lock off
    // (stock's activity is locked to landscape; the UI's elements turn instead). The lock
    // and the transform it had are given back when the camera leaves the front or closes.
    fn hold_front(&self, front: bool) {
        let Some(window) = self.view.root().and_downcast::<gtk::Window>() else { return };
        let Some(app) = window.application() else { return };
        if front && self.idle_cookie.get() == 0 {
            self.idle_cookie.set(app.inhibit(Some(&window), gtk::ApplicationInhibitFlags::IDLE, Some("Taking photos")));
            front_marker(true);
        } else if !front && self.idle_cookie.get() != 0 {
            app.uninhibit(self.idle_cookie.replace(0));
            front_marker(false);
        }
        let held = self.landscape_held.borrow().is_some();
        if front && !held {
            let Some(lock) = rotation_lock() else { return };
            let was = lock.boolean("orientation-lock");
            let transform = display_transform();
            let _ = lock.set_boolean("orientation-lock", true);
            if transform.as_deref() != Some("270") {
                set_display_transform("270");
            }
            eprintln!("nebula: display held in landscape (was lock {was}, transform {transform:?})");
            *self.landscape_held.borrow_mut() = Some((was, transform));
        } else if !front && held {
            self.release_landscape();
        }
    }

    fn release_landscape(&self) {
        front_marker(false);
        let Some((was, transform)) = self.landscape_held.borrow_mut().take() else { return };
        if was {
            // locked before: as it was locked
            if let Some(t) = transform.filter(|t| t != "270") {
                set_display_transform(&t);
            }
        }
        if let Some(lock) = rotation_lock() {
            let _ = lock.set_boolean("orientation-lock", was);
        }
        eprintln!("nebula: display given back (lock {was})");
    }

    // AF-D as stock's app runs it (SmartAFTriggerMgr; there is no ASIC mode for stills): the
    // centre is focused again once the camera has moved and settled (the gyro), or the zoom
    // has changed; not for 1.5 s after a focus by hand. (Stock also follows faces.)
    fn continuous_focus(self: &Rc<Self>) {
        let (on, want, zoom, last) = {
            let st = self.st.borrow();
            let on = st.caf && st.mode != Mode::Manual;
            let settled = st.settle.is_none() && st.switching.is_none();
            let paused = st.caf_pause_until.is_some_and(|t| Instant::now() < t);
            (on, on && !st.busy && settled && !paused, st.zoom, st.caf_zoom)
        };
        // a move while AF-D is off doesn't count; one during the pause after a focus by hand
        // (or a busy moment) is kept until AF-D may act on it, not lost
        if !on {
            self.moved.store(false, Ordering::Relaxed);
            return;
        }
        if !want || self.focusing.load(Ordering::SeqCst) {
            return;
        }
        let moved = self.moved.swap(false, Ordering::Relaxed);
        if moved || last.is_some_and(|z| (zoom - z).abs() > 1.0) {
            self.focus_run(None, true);
        }
    }

    fn on_input(self: &Rc<Self>, ev: input::Ev) {
        // the preview stopped behind another app or the settings: a button brings it back
        if self.st.borrow().asleep {
            if let input::Ev::Key(input::KEY_CAMERA_FOCUS | input::KEY_CAMERA, true) = ev {
                self.settings_page.set_visible(false);
                if let Some(w) = self.view.root().and_downcast::<gtk::Window>() {
                    w.present();
                }
                self.follow_screen();
            }
            return;
        }
        match ev {
            input::Ev::Key(input::KEY_CAMERA_FOCUS, true) => self.focus(self.last_focus_point()),
            input::Ev::Key(input::KEY_CAMERA | input::KEY_VOLUMEUP, true) => self.shutter_pressed(),
            input::Ev::Key(..) => {}
            // the strip's position comes before its touch-down in each report
            input::Ev::StripX(_) | input::Ev::StripTouch(_) if !self.st.borrow().strip_zoom => {}
            input::Ev::StripX(x) => {
                let (down, last, active, lock, x0) = {
                    let st = self.st.borrow();
                    (st.strip_down, st.strip_x, st.strip_active, st.strip_lock, st.strip_x0)
                };
                if !down {
                    // a touch begins: the function it will have is fixed now, and nothing moves
                    // until the finger has gone a clear way (a tap or a double tap never nudges a value)
                    let function = self.strip_function();
                    let mut st = self.st.borrow_mut();
                    st.strip_down = true;
                    st.strip_active = false;
                    st.strip_lock = function;
                    st.strip_t0 = Instant::now();
                    st.strip_x0 = x;
                    st.strip_x = x;
                } else if !active {
                    if ((x - x0).abs() as f64) >= STRIP_LEN / 20.0 {
                        // the slide begins here
                        {
                            let mut st = self.st.borrow_mut();
                            st.strip_active = true;
                            st.strip_x = x;
                        }
                        if lock != 0 {
                            self.wheel_grab([Dial::Iso, Dial::Shutter, Dial::Ev][lock - 1]);
                        }
                    }
                } else {
                    self.st.borrow_mut().strip_x = x;
                    if lock == 0 {
                        // OpenLight: a full strip length zooms 2.3x
                        let raw = self.st.borrow().zoom_raw;
                        self.zoom_gesture(raw * 2.3f64.powf((x - last) as f64 / STRIP_LEN));
                    } else {
                        // a full strip length is most of the value's range; to the right for more
                        // (a greater position is a lower value)
                        let dial = [Dial::Iso, Dial::Shutter, Dial::Ev][lock - 1];
                        let (pos, inverse) = {
                            let st = self.st.borrow();
                            (
                                match dial {
                                    Dial::Iso => st.iso,
                                    Dial::Shutter => st.shutter,
                                    Dial::Ev => st.ev,
                                },
                                if st.inverse_wheel { -1.0 } else { 1.0 },
                            )
                        };
                        self.set_dial(dial, pos - inverse * (x - last) as f64 / STRIP_LEN * 0.8);
                    }
                }
            }
            input::Ev::StripTouch(true) => {}
            input::Ev::StripTouch(false) => {
                let (active, lock, x0, quick) = {
                    let mut st = self.st.borrow_mut();
                    st.strip_down = false;
                    let active = std::mem::replace(&mut st.strip_active, false);
                    if active {
                        st.strip_slid_at = Some(Instant::now());
                        st.strip_tap_at = None;
                    }
                    (active, st.strip_lock, st.strip_x0, st.strip_t0.elapsed() < Duration::from_millis(300))
                };
                if active {
                    // a slide ended: not a tap, and no tap right after it counts either
                    if lock != 0 {
                        self.wheel_release();
                    }
                    return;
                }
                if !quick {
                    return;
                }
                // taps on the ends step between the primes (as the zoom)
                if x0 < 100 && lock == 0 {
                    self.step_prime(false);
                } else if x0 > 700 && lock == 0 {
                    self.step_prime(true);
                } else if (100..=700).contains(&x0) {
                    // a double tap in the middle: the strip's next function (not just after a slide)
                    let double = {
                        let mut st = self.st.borrow_mut();
                        let after_slide = st.strip_slid_at.is_some_and(|t| t.elapsed() < Duration::from_millis(250));
                        let double = !after_slide && st.strip_tap_at.is_some_and(|t| t.elapsed() < Duration::from_millis(420));
                        st.strip_tap_at = if double || after_slide { None } else { Some(Instant::now()) };
                        double
                    };
                    if double {
                        self.cycle_strip();
                    }
                }
            }
        }
    }

    // the preview's brightness (Rec. 601 luma, 64 bins) from its current frame, sampled; with
    // the preview's digital gain, as shown
    // the preview's frame, drawn into a texture of its own (keeping the sink's frame would
    // keep one of libcamera's few buffers from the camera)
    fn preview_still(&self, w: f32, h: f32) -> Option<gdk::Texture> {
        let renderer = self.view.native().and_then(|n| n.renderer())?;
        let snap = gtk::Snapshot::new();
        self.paintable.snapshot(&snap, w as f64, h as f64);
        let node = snap.to_node()?;
        Some(renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, w, h))))
    }

    fn update_histogram(&self) {
        // the frame as drawn, small: the sink's current image isn't always a plain texture,
        // and 160x120 is plenty for 64 bins
        let Some(renderer) = self.view.native().and_then(|n| n.renderer()) else { return };
        let (w, h) = (160.0f32, 120.0f32);
        let snap = gtk::Snapshot::new();
        self.paintable.snapshot(&snap, w as f64, h as f64);
        let Some(node) = snap.to_node() else { return };
        let tex = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, w, h)));
        let (tw, th) = (tex.width() as usize, tex.height() as usize);
        let mut buf = vec![0u8; tw * th * 4];
        tex.download(&mut buf, tw * 4);
        // red, green, blue and luma, 64 bins each
        let mut bins = vec![0u32; HIST_BINS * 4];
        let bin = |v: f64| ((v / 256.0 * HIST_BINS as f64) as usize).min(HIST_BINS - 1);
        for p in buf.chunks_exact(4) {
            // GDK's download format is B8G8R8A8 (premultiplied; the preview is opaque)
            let (r, g, b) = (p[2] as f64, p[1] as f64, p[0] as f64);
            bins[bin(r)] += 1;
            bins[HIST_BINS + bin(g)] += 1;
            bins[2 * HIST_BINS + bin(b)] += 1;
            bins[3 * HIST_BINS + bin(r * 0.299 + g * 0.587 + b * 0.114)] += 1;
        }
        *self.hist.borrow_mut() = bins;
    }

    // The histogram as an instrument: luma filled, the colour channels as lines over it, the
    // zones (stops of the 0-255 range) ticked, and the ends lit red when pixels clip there
    fn draw_histogram(&self, cr: &cairo::Context) {
        let bins = self.hist.borrow();
        let (x0, y0, bw, bh) = (16.0, 16.0, 192.0, 72.0);
        rounded(cr, x0, y0, bw, bh, 10.0);
        cr.set_source_rgba(0.04, 0.04, 0.05, 0.72);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        let (px, py, pw, ph) = (x0 + 8.0, y0 + 8.0, bw - 16.0, bh - 22.0);
        let channel = |c: usize| &bins[c * HIST_BINS..(c + 1) * HIST_BINS];
        let total: f64 = channel(3).iter().map(|&n| n as f64).sum::<f64>().max(1.0);
        // one scale for all four, from the busiest bin that isn't a clipped end (a clipped sky
        // would flatten the rest)
        let max = (0..4)
            .flat_map(|c| channel(c)[1..HIST_BINS - 1].iter().copied())
            .max()
            .unwrap_or(0)
            .max(1) as f64;
        let step = pw / HIST_BINS as f64;
        let height = |n: u32| ((n as f64 / max).sqrt()).min(1.0) * ph;
        // zones
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.10);
        cr.set_line_width(1.0);
        for z in 1..4 {
            let x = (px + pw * z as f64 / 4.0).round() + 0.5;
            cr.move_to(x, py);
            cr.line_to(x, py + ph);
        }
        let _ = cr.stroke();
        // luma
        cr.move_to(px, py + ph);
        for (i, &n) in channel(3).iter().enumerate() {
            cr.line_to(px + (i as f64 + 0.5) * step, py + ph - height(n));
        }
        cr.line_to(px + pw, py + ph);
        cr.close_path();
        cr.set_source_rgba(0.95, 0.95, 0.92, 0.55);
        let _ = cr.fill();
        // red, green, blue
        cr.set_line_width(1.2);
        for (c, rgb) in [(0, (1.0, 0.32, 0.30)), (1, (0.36, 0.89, 0.49)), (2, (0.36, 0.60, 1.0))] {
            for (i, &n) in channel(c).iter().enumerate() {
                let (x, y) = (px + (i as f64 + 0.5) * step, py + ph - height(n));
                if i == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            cr.set_source_rgba(rgb.0, rgb.1, rgb.2, 0.9);
            let _ = cr.stroke();
        }
        // clipping: a channel with over half a percent in its end bin
        let clip = |end: usize| (0..3).any(|c| channel(c)[end] as f64 / total > 0.005);
        for (end, x) in [(0, px), (HIST_BINS - 1, px + pw - 3.0)] {
            if clip(end) {
                cr.set_source_rgb(1.0, 0.30, 0.30);
                rounded(cr, x, py, 3.0, ph, 1.5);
                let _ = cr.fill();
            }
        }
        // the labels under it: the shadows' and the highlights' share
        let share = |range: std::ops::Range<usize>| {
            range.map(|i| channel(3)[i] as f64).sum::<f64>() / total * 100.0
        };
        cr.select_font_face("Adwaita Mono", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        let (lo, hi) = (share(0..HIST_BINS / 8), share(HIST_BINS - HIST_BINS / 8..HIST_BINS));
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.55);
        text(cr, &format!("SHD {lo:.0}%"), px, y0 + bh - 8.0, 10.0, 0.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.55);
        text(cr, &format!("HLT {hi:.0}%"), px + pw, y0 + bh - 8.0, 10.0, 1.0);
    }

    fn draw_marks(&self, cr: &cairo::Context, w: f64, h: f64) {
        let st = self.st.borrow();
        if st.grid > 0 {
            // thirds, or the golden ratio's lines (0.382 and 0.618 of the way across)
            let at = if st.grid == 1 { [1.0 / 3.0, 2.0 / 3.0] } else { [0.382, 0.618] };
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.4);
            cr.set_line_width(1.0);
            for f in at {
                let x = (w * f).round() + 0.5;
                let y = (h * f).round() + 0.5;
                cr.move_to(x, 0.0);
                cr.line_to(x, h);
                cr.move_to(0.0, y);
                cr.line_to(w, y);
            }
            let _ = cr.stroke();
        }
    }

    // stock's focus marks (CrossHair): grey corners while focusing; then yellow and a little
    // larger when focused, with a lock while the focus holds (no AF-D), or a shake when
    // not; dimmed after a second, gone after five
    fn draw_focus(&self, cr: &cairo::Context, st: &State, centre: (f64, f64)) {
        let (mut cx, cy) = centre;
        let now = Instant::now();
        let (mut s, mut alpha) = (40.0, 1.0);
        let mut yellow = false;
        if let Some((r, at)) = st.focus_done {
            let t = now.duration_since(at).as_secs_f64();
            if r == 1 {
                yellow = true;
                s *= 1.0 + 0.1 * (t / 0.1).min(1.0);
            } else if t < 0.9 {
                // stock's shake: 25 px, dying away over 0.9 s
                cx += 25.0 * (1.0 - t / 0.9) * (t * 2.0 * PI * 5.0).sin();
            }
            if t > 1.0 {
                alpha = 1.0 - (1.0 - 90.0 / 255.0) * ((t - 1.0) / 0.1).min(1.0);
            }
        }
        let c = s * 0.3;
        let (r, g, b) = if yellow { (1.0, 0.812, 0.404) } else { (0.85, 0.85, 0.85) };
        for (width, rgb) in [(5.0, (0.29, 0.29, 0.29)), (3.0, (r, g, b))] {
            cr.set_source_rgba(rgb.0, rgb.1, rgb.2, alpha);
            cr.set_line_width(width);
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                cr.move_to(cx + sx * s, cy + sy * (s - c));
                cr.line_to(cx + sx * s, cy + sy * s);
                cr.line_to(cx + sx * (s - c), cy + sy * s);
            }
            let _ = cr.stroke();
        }
        if yellow && !st.caf {
            cr.set_source_rgba(r, g, b, alpha);
            text(cr, &icons::LOCK.to_string(), cx, cy - s - 4.0, 22.0, 0.5);
        }
    }

    // the adjust panel: the value in dot matrix over a number line, beside the encoder that is
    // held, or (for the zoom, which has just changed) at the preview's bottom; it fades in and out
    fn update_wheels(&self) {
        let was_off = self.flyout_turn.has_css_class("off");
        let mut shown: Option<u8> = None;
        {
            let st = self.st.borrow();
            if let Some(dial) = st.wheel {
                let (key, pos, value) = match dial {
                    Dial::Iso => (1, st.iso, iso_at(st.iso).to_string()),
                    Dial::Shutter => (2, st.shutter, fmt_secs(secs_at(st.shutter))),
                    Dial::Ev => (3, st.ev, fmt_ev(ev_at(st.ev))),
                };
                self.wheels.configure(key, || exposure_spec(dial));
                self.wheels.set(pos);
                self.flyout_unit.set(dial_name(dial));
                self.flyout_suffix.set("");
                self.set_flyout_text(&value, [4, 6, 4][dial_index(dial)], 1.0 - pos);
                shown = Some(dial_index(dial) as u8);
            } else if st.zoom_wheel_until.is_some_and(|t| Instant::now() < t) {
                let pos = (st.zoom / ZOOM_MIN).ln();
                self.wheels.configure(20, zoom_spec);
                self.wheels.set(pos);
                self.flyout_unit.set("FOCAL LENGTH");
                self.flyout_suffix.set("mm");
                self.set_flyout_text(&format!("{:.0}", st.zoom), 3, pos);
                shown = Some(3);
            }
        }
        match shown {
            Some(at) => {
                if self.flyout_at.get() != at {
                    self.place_flyout(at);
                }
                if was_off {
                    self.wheels.jump();
                }
            }
            None => self.flyout_at.set(255),
        }
        set_class(&self.flyout_turn, "off", shown.is_none());
        for (k, c) in self.encoders.iter().enumerate() {
            set_class(c, "held", shown == Some(k as u8));
        }
    }

    // ---- the mode picker, the bubble, the preset

    fn picker_open(&self) -> bool {
        !self.picker_turn.has_css_class("off")
    }

    // the picker beside the mode key: to its left, over the preview
    fn open_picker(self: &Rc<Self>) {
        let cur = self.st.borrow().mode.index();
        for (i, b) in self.picker_rows.iter().enumerate() {
            set_class(b, "on", i == cur);
        }
        let Some(b) = self.mode_btn.compute_bounds(&self.root) else { return };
        let (_, nat) = self.picker_card.preferred_size();
        let (w, h) = if self.quarter.get() == 0 { (nat.width() as f32, nat.height() as f32) } else { (nat.height() as f32, nat.width() as f32) };
        let max_y = (self.root.height() as f32 - h - 6.0).max(6.0);
        self.picker_turn.set_margin_start((b.x() - w - 10.0).max(6.0) as i32);
        self.picker_turn.set_margin_top(b.y().clamp(6.0, max_y) as i32);
        set_class(&self.picker_turn, "off", false);
        self.picker_turn.set_can_target(true);
        let a = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_secs(6), move || {
            *a.picker_timer.borrow_mut() = None;
            a.close_picker();
        });
        if let Some(old) = self.picker_timer.borrow_mut().replace(id) {
            old.remove();
        }
    }

    fn close_picker(&self) {
        set_class(&self.picker_turn, "off", true);
        self.picker_turn.set_can_target(false);
        if let Some(id) = self.picker_timer.borrow_mut().take() {
            id.remove();
        }
    }

    // a badge shown or hidden; when it appears, its description as an alert (not again for 30 s)
    fn set_badge(self: &Rc<Self>, key: &'static str, badge: &gtk::Label, on: bool, icon: char, title: &str, text: &str) {
        if on && !badge.is_visible() {
            let fresh = self.hint_at.borrow().get(key).is_none_or(|t| t.elapsed() > Duration::from_secs(30));
            if fresh {
                self.hint_at.borrow_mut().insert(key, Instant::now());
                self.show_alert(key, icon, title, text, 4);
            }
        }
        badge.set_visible(on);
    }

    // what the key says about its choice now
    fn tool_note(&self, t: Tool) -> (String, String) {
        let st = self.st.borrow();
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        match t {
            Tool::Flash => [s("Flash off", "It never fires."), s("Flash auto", "It fires when the scene is dark."), s("Flash on", "It fires on every photo.")][st.flash as usize].clone(),
            Tool::Wb => {
                let notes = ["Matches the light automatically.", "Warm indoor bulbs.", "Office tube lights.", "Sun, and flash.", "Overcast sky: a little warmer."];
                (format!("White balance: {}", wb::PRESETS[st.wb]), notes[st.wb.min(4)].to_string())
            }
            Tool::Timer => {
                let secs = TIMERS[st.timer];
                if secs == 0 { s("Timer off", "The photo is taken at once.") } else { (format!("Timer {secs} s"), format!("The photo is taken {secs} seconds after the shutter.")) }
            }
            Tool::Grid => [s("Grid off", "No lines over the preview."), s("Grid 3 x 3", "Rule of thirds."), s("Grid golden", "The golden ratio, phi.")][st.grid as usize].clone(),
            Tool::Histogram => if st.histogram { s("Histogram on", "The tones of the live image, with clipping marked red.") } else { s("Histogram off", "") },
            Tool::Assist => [s("Assist off", "No overlay."), s("Focus peaking", "Green outlines show what is sharp."), s("Zebras", "Stripes mark highlights about to clip."), s("Peaking and zebras", "Green for sharp, stripes for clipping.")][st.assist as usize].clone(),
            Tool::Burst => { let b = BURSTS[st.burst]; if b > 1 { (format!("Burst of {b}"), format!("{b} photos in a row from one press.")) } else { s("Burst off", "One photo a press.") } }
            Tool::Afd => if st.caf { s("Continuous focus on", "It refocuses when the scene changes.") } else { s("Continuous focus off", "It focuses when you tap.") },
            Tool::Meter => [s("Centre-weighted", "It meters the middle of the frame."), s("Touch metering", "It meters where you tap."), s("Whole frame", "It meters the whole frame.")][st.metering as usize].clone(),
            Tool::Geo => if st.geotag { s("Geotag on", "Photos record where they were taken.") } else { s("Geotag off", "Photos carry no location.") },
            Tool::Strip => (format!("Touch strip: {}", STRIP_FNS[st.strip_fn].1), "Slide along it to change this. Double tap it to switch.".to_string()),
        }
    }

    // what the strip does now: its function, or zoom when the mode no longer lets you set it
    fn strip_function(&self) -> usize {
        let st = self.st.borrow();
        match st.strip_fn {
            1 if !st.mode.fixes_iso() => 0,
            2 if !st.mode.fixes_shutter() => 0,
            3 if st.mode == Mode::Manual => 0,
            f => f,
        }
    }

    // a double tap on the strip (or its key): the next of the chosen functions that the mode allows
    fn cycle_strip(self: &Rc<Self>) {
        let (cur, set) = (self.strip_function(), self.st.borrow().strip_set);
        let mode = self.st.borrow().mode;
        let allowed = |k: usize| {
            set >> k & 1 == 1
                && match k {
                    1 => mode.fixes_iso(),
                    2 => mode.fixes_shutter(),
                    3 => mode != Mode::Manual,
                    _ => true,
                }
        };
        let next = (1..=4).map(|d| (cur + d) % 4).find(|&k| allowed(k)).unwrap_or(0);
        self.st.borrow_mut().strip_fn = next;
        self.refresh();
        self.buzz(15);
        self.show_pill(icons::STRIP, &format!("Touch strip: {}", STRIP_FNS[next].1), 2);
    }

    // ---- the controls swiped away, pinned keys, the system panel

    // the unpinned keys stowed (slid right and faded) or back; the pinned ones stay
    fn apply_stow(&self) {
        let (pinned, stowed) = (self.st.borrow().pinned, self.stowed.get());
        for (k, key) in self.keys.iter().enumerate() {
            let pin = pinned >> k & 1 == 1;
            set_class(key, "pinned", pin);
            let hide = stowed && !pin;
            set_class(key, "stowed", hide);
            key.set_can_target(!hide);
        }
    }

    // the controls away: the unpinned keys fade, then the pinned ones gather in a strip beside the
    // shutter and the preview grows into the room (with the lens strip over its foot); or back
    fn set_stowed(self: &Rc<Self>, on: bool) {
        if self.stowed.replace(on) == on {
            return;
        }
        self.buzz(10);
        if on {
            self.apply_stow();
            self.show_pill(icons::CHEVRON_RIGHT, "Controls hidden: swipe left to bring them back", 3);
            let a = self.clone();
            glib::timeout_add_local_once(Duration::from_millis(230), move || {
                if a.stowed.get() {
                    a.enter_compact();
                    a.animate_deck(1.0);
                }
            });
        } else {
            self.leave_compact();
            self.apply_stow();
            self.animate_deck(0.0);
        }
    }

    // how many pinned keys fit in the narrow column: its height less the gallery, the shutter
    // and the gaps, in keys
    fn pin_cap(&self) -> usize {
        let h = self.root.height() as f64;
        if h < 100.0 {
            return 3;
        }
        let room = h - 20.0 - SHUTTER_W - SHUTTER_W - 20.0 + 8.0;
        ((room / (KEY_H + 8.0)).floor() as usize).max(1)
    }

    // the pinned keys out of the grid and into the strip at the shutter's width; the gallery
    // above the shutter, both in the narrow column
    fn enter_compact(&self) {
        if self.compact.replace(true) {
            return;
        }
        let (pinned, cap) = (self.st.borrow().pinned, self.pin_cap());
        let mut placed = 0;
        for (k, key) in self.keys.iter().enumerate() {
            if pinned >> k & 1 == 1 && placed < cap {
                placed += 1;
                self.key_grid.remove(key);
                key.set_size_request(SHUTTER_W as i32, -1);
                self.pin_strip.append(key);
            }
        }
        self.key_grid.set_visible(false);
        self.pin_strip.set_visible(true);
        self.shutter_row.set_orientation(gtk::Orientation::Vertical);
        self.shutter_row.set_spacing(10);
        self.shutter_fill.set_visible(false);
        self.thumb.set_pixel_size(SHUTTER_W as i32);
    }

    fn leave_compact(&self) {
        if !self.compact.replace(false) {
            return;
        }
        for (k, key) in self.keys.iter().enumerate() {
            if key.parent().is_some_and(|p| p == *self.pin_strip.upcast_ref::<gtk::Widget>()) {
                self.pin_strip.remove(key);
                key.set_size_request(-1, -1);
                self.key_grid.attach(key, (k % 3) as i32, (k / 3) as i32, 1, 1);
            }
        }
        self.pin_strip.set_visible(false);
        self.key_grid.set_visible(true);
        self.shutter_row.set_orientation(gtk::Orientation::Horizontal);
        self.shutter_row.set_spacing(0);
        self.shutter_fill.set_visible(true);
        self.thumb.set_pixel_size(72);
    }

    // the layout now: the controls out or stowed (stow_t), and the overheating warning's room
    fn apply_deck(&self) {
        let t = self.stow_t.get();
        let t = t * t * (3.0 - 2.0 * t);
        let lerp = |a: f64, b: f64| a + (b - a) * t;
        let warm = THERMAL_ROOM * self.thermal_t.get();
        self.right.set_width_request(lerp(RIGHT_W, STOW_W) as i32);
        self.centre.set_margin_end(lerp(RESERVE_FULL, RESERVE_STOW) as i32);
        self.frame.set_margin_bottom((lerp(LENS_ROOM, 0.0) + warm) as i32);
        self.zoom_pill.set_margin_bottom((10.0 + warm) as i32);
    }

    // there in about a quarter of a second, on a smooth curve
    fn animate_deck(self: &Rc<Self>, to: f64) {
        if let Some(id) = self.stow_anim.borrow_mut().take() {
            id.remove();
        }
        let a = self.clone();
        let last = Cell::new(0i64);
        let id = self.right.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let before = last.replace(now);
            let dt = if before == 0 { 0.016 } else { ((now - before) as f64 / 1e6).clamp(0.001, 0.05) };
            let cur = a.stow_t.get();
            let step = dt / 0.26;
            let next = if to > cur { (cur + step).min(to) } else { (cur - step).max(to) };
            a.stow_t.set(next);
            a.apply_deck();
            if next == to {
                a.stow_anim.replace(None);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.stow_anim.borrow_mut() = Some(id);
    }

    fn toggle_pin(self: &Rc<Self>, k: usize) {
        let (pinned, cap) = (self.st.borrow().pinned, self.pin_cap());
        if pinned >> k & 1 == 0 && pinned.count_ones() as usize >= cap {
            self.buzz(30);
            self.show_pill(icons::LOCK, &format!("No room for more than {cap} pinned keys"), 3);
            return;
        }
        let now = {
            let mut st = self.st.borrow_mut();
            st.pinned ^= 1 << k;
            st.pinned >> k & 1 == 1
        };
        self.buzz(20);
        if self.compact.get() {
            self.leave_compact();
            self.enter_compact();
        }
        self.refresh();
        if now {
            self.show_pill(icons::LOCK, "Pinned: it stays when the controls hide", 2);
        } else {
            self.show_pill(icons::LOCK, "Unpinned", 2);
        }
    }

    // ---- the sidebar and the settings screen

    // the app's and the system's versions, for the About pane and the sidebar's foot
    fn about_lines(&self) -> Vec<String> {
        let os = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_else(|| "unknown system".into());
        let mut u: libc::utsname = unsafe { std::mem::zeroed() };
        let kernel = if unsafe { libc::uname(&mut u) } == 0 {
            let r = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) };
            format!("Linux {}", r.to_string_lossy())
        } else {
            "Linux".into()
        };
        vec![
            format!("Nebula  v{}", env!("CARGO_PKG_VERSION")),
            os,
            kernel,
            gst::version_string().to_string(),
            if self.ccb.is_some() { "camera driver: light-ccb".into() } else { "no camera driver".into() },
        ]
    }

    // the sidebar in or out (the dimmed layer with it)
    fn show_sidebar(self: &Rc<Self>, on: bool) {
        if on == !self.side_panel.has_css_class("side-hidden") {
            return;
        }
        set_class(&self.side_panel, "side-hidden", !on);
        set_class(&self.scrim, "off", !on);
        self.side_panel.set_can_target(on);
        self.scrim.set_can_target(on);
        if on {
            self.fill_sidebar();
            self.buzz(10);
        }
    }

    // the quick checkboxes, from the state as it is now, and the About lines at the foot
    fn fill_sidebar(self: &Rc<Self>) {
        while let Some(c) = self.side_quick.first_child() {
            self.side_quick.remove(&c);
        }
        let items: [(&str, fn(&State) -> bool, fn(&mut State, bool)); 4] = [
            ("High contrast", |s| s.contrast == 1, |s, v| s.contrast = if v { 1 } else { 0 }),
            ("Geotagging", |s| s.geotag, |s, v| s.geotag = v),
            ("Shutter sparkle", |s| s.sparkle, |s, v| s.sparkle = v),
            ("Haptics", |s| s.haptics != 0, |s, v| s.haptics = if v { 1 } else { 0 }),
        ];
        for (name, get, set) in items {
            let a = self.clone();
            self.side_quick.append(&settings_ui::check(name, get(&self.st.borrow()), move |v| {
                set(&mut a.st.borrow_mut(), v);
                a.setting_changed();
            }));
        }
        let lines = self.about_lines();
        self.side_about.set_text(&format!("{}\n{}\n{}", lines[0], lines[1], lines[2]));
    }

    // the settings screen: the categories, the first one shown
    fn fill_settings(self: &Rc<Self>) {
        while let Some(c) = self.settings_nav.first_child() {
            self.settings_nav.remove(&c);
        }
        for section in settings_ui::SECTIONS {
            let l = gtk::Label::new(Some(section.name));
            l.set_xalign(0.0);
            self.settings_nav.append(&l);
        }
        if let Some(row) = self.settings_nav.row_at_index(0) {
            self.settings_nav.select_row(Some(&row));
        }
        settings_ui::fill_pane(self, &self.settings_pane, 0);
    }

    // the settings page in (it fades and slides in) or out; hidden after its outro
    fn open_settings(self: &Rc<Self>) {
        self.fill_settings();
        self.settings_page.set_visible(true);
        let a = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(30), move || set_class(&a.settings_page, "page-off", false));
        self.follow_screen();
    }

    fn close_settings(self: &Rc<Self>) {
        if !self.settings_page.is_visible() {
            return;
        }
        set_class(&self.settings_page, "page-off", true);
        let a = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(260), move || {
            if a.settings_page.has_css_class("page-off") {
                a.settings_page.set_visible(false);
                a.follow_screen();
            }
        });
    }

    fn dial_active(&self, dial: Dial) -> bool {
        let mode = self.st.borrow().mode;
        match dial {
            Dial::Iso => mode.fixes_iso(),
            Dial::Shutter => mode.fixes_shutter(),
            Dial::Ev => mode != Mode::Manual,
        }
    }

    fn wheel_grab(self: &Rc<Self>, dial: Dial) {
        {
            let mut st = self.st.borrow_mut();
            if let Some(id) = st.wheel_close.take() {
                id.remove();
            }
            st.wheel = Some(dial);
            st.wheel_start = match dial {
                Dial::Iso => st.iso,
                Dial::Shutter => st.shutter,
                Dial::Ev => st.ev,
            };
        }
        self.place_flyout(dial_index(dial) as u8);
        self.buzz(15);
        self.refresh();
        self.update_wheels();
    }

    // the finger @dy down the screen: the old swipe, up for more and down for less (a greater
    // position is a lower ISO or shutter value)
    fn wheel_drag(&self, dy: f64) {
        let (dial, start, dir) = {
            let st = self.st.borrow();
            (st.wheel, st.wheel_start, if st.inverse_wheel { -1.0 } else { 1.0 })
        };
        if let Some(dial) = dial {
            self.set_dial(dial, start + dir * dy * 0.001);
        }
    }

    // the finger lifted: the ruler stays a moment
    fn wheel_release(self: &Rc<Self>) {
        if self.st.borrow().wheel.is_none() {
            return;
        }
        self.buzz(10);
        let b = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(600), move || {
            let mut st = b.st.borrow_mut();
            st.wheel_close = None;
            st.wheel = None;
            drop(st);
            b.refresh();
            b.update_wheels();
        });
        if let Some(old) = self.st.borrow_mut().wheel_close.replace(id) {
            old.remove();
        }
    }

    // an encoder: a ring of dots for where the value is in its range, the name inside it, the
    // value in dot matrix under it (left-justified in its cells). Accent when the mode has it in
    // hand; white, dim and tagged AUTO when it is the camera's own (metered) value
    fn draw_encoder(&self, cr: &cairo::Context, w: f64, h: f64, dial: Dial) {
        let st = self.st.borrow();
        let (name, value, frac, active) = encoder_shows(&st, dial);
        let on = if active { (accent().0, accent().1, accent().2, 1.0) } else { (0.95, 0.95, 0.93, if contrast() { 0.95 } else { 0.62 }) };
        let (cx, cy, r) = (w / 2.0, h * 0.36, w.min(h) * 0.27);
        let n = 36;
        let lit = (frac.clamp(0.0, 1.0) * n as f64).round() as usize;
        for k in 0..n {
            let a = (135.0 + k as f64 * 270.0 / (n - 1) as f64).to_radians();
            let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
            if k < lit {
                cr.set_source_rgba(on.0, on.1, on.2, on.3);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, if contrast() { 0.32 } else { 0.13 });
            }
            cr.arc(x, y, 1.7, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
        cr.select_font_face("Adwaita Mono", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_source_rgba(on.0, on.1, on.2, if active { 1.0 } else { 0.7 });
        text_at(cr, name, cx, cy, if name.len() > 4 { 11.0 } else { 14.0 });
        if !active {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
            text(cr, "AUTO", w - 10.0, 14.0, 9.0, 1.0);
        }
        self.enc_roll[dial_index(dial)].draw(cr, &value, [4, 6, 4][dial_index(dial)], 14.0, cy + r + 14.0, 3.0, on);
    }

    // the flyout: what is being set, large, in dot matrix, left-justified in its cells
    fn draw_flyout(&self, cr: &cairo::Context, _w: f64, h: f64) {
        cr.select_font_face("Adwaita Mono", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.5);
        text(cr, self.flyout_unit.get(), 20.0, 20.0, 14.0, 0.0);
        let cells = self.flyout_cells.get();
        let pitch = 5.0;
        let top = 28.0 + (h - 28.0 - 7.0 * pitch) / 2.0 - 2.0;
        let on = (accent().0, accent().1, accent().2, 1.0);
        self.flyout_roll.draw(cr, "", cells, 20.0, top, pitch, on);
        // a suffix (mm) sits after the cells
        let suffix = self.flyout_suffix.get();
        if !suffix.is_empty() {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.7);
            text(cr, suffix, 20.0 + dots::cells_width(cells, pitch) + 12.0, top + 7.0 * pitch - 6.0, 18.0, 0.0);
        }
    }

    // the flyout's value, in @cells cells, rolling from the last one; @up is the value's position
    fn set_flyout_text(&self, value: &str, cells: usize, up: f64) {
        self.flyout_cells.set(cells);
        self.flyout_roll.set(&self.flyout, value, up);
    }

    // the panel, in place: beside the encoder of a dial (to its right, on the preview, where the
    // thumb that holds the encoder does not cover it), or for the zoom at the preview's bottom
    // centre. @at: 0-2 an encoder, 3 the zoom
    fn place_flyout(&self, at: u8) {
        let (fw, fh) = if self.quarter.get() == 0 { (FLYOUT.0 as f32, FLYOUT.1 as f32) } else { (FLYOUT.1 as f32, FLYOUT.0 as f32) };
        let (x, y) = if at < 3 {
            let Some(b) = self.encoders[at as usize].compute_bounds(&self.root) else { return };
            (b.x() + b.width() + 10.0, b.y() + (b.height() - fh) / 2.0)
        } else {
            let Some(b) = self.view.compute_bounds(&self.root) else { return };
            (b.x() + (b.width() - fw) / 2.0, b.y() + b.height() - fh - 16.0)
        };
        self.flyout_turn.set_margin_start(x.max(4.0) as i32);
        self.flyout_turn.set_margin_top(y.max(4.0) as i32);
        self.flyout_at.set(at);
    }

    // the shutter: a ring of dots round a light disc; when a photo is taken a light runs round the ring
    // (in the accent colour); the whole dims while photos save
    fn draw_shutter(&self, cr: &cairo::Context, w: f64, h: f64) {
        let busy = self.st.borrow().busy;
        let (cx, cy, r) = (w / 2.0, h / 2.0, w.min(h) / 2.0 - 4.0);
        let t = self.shutter_flash.get().map_or(1.0, |t0| (t0.elapsed().as_secs_f64() / 0.55).min(1.0));
        let n = 40;
        let (ar, ag, ab) = accent();
        for k in 0..n {
            let a = (k as f64 / n as f64 * 2.0 - 0.5) * PI;
            let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
            // the light: the dots up to t round the ring, then everything eases back
            let f = k as f64 / n as f64;
            let lit = if t < 1.0 { (1.0 - ((t - f).abs() * 6.0).min(1.0)).max(if f < t { (1.0 - t) * 1.6 } else { 0.0 }) } else { 0.0 };
            let base = if busy { 0.28 } else { 0.7 };
            cr.set_source_rgba(
                base + (ar - base) * lit,
                base + (ag - base) * lit,
                base + (ab - base) * lit,
                if busy { 0.5 } else { 0.9 },
            );
            cr.arc(x, y, 2.0, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
        cr.set_source_rgba(0.95, 0.95, 0.93, if busy { 0.3 } else { 1.0 });
        cr.arc(cx, cy, r - 12.0, 0.0, 2.0 * PI);
        let _ = cr.fill();
    }

    // the light round the shutter's ring, for half a second
    fn flash_shutter(self: &Rc<Self>) {
        self.shutter_flash.set(Some(Instant::now()));
        let a = self.clone();
        self.shutter.add_tick_callback(move |w, _| {
            if let Some(c) = w.downcast_ref::<Canvas>() {
                c.queue_draw();
            }
            if a.shutter_flash.get().is_some_and(|t| t.elapsed().as_secs_f64() < 0.56) {
                glib::ControlFlow::Continue
            } else {
                a.shutter_flash.set(None);
                glib::ControlFlow::Break
            }
        });
    }
}

// iio-sensor-proxy's ambient light (lux), claimed for as long as the app runs
fn light_proxy() -> Option<gtk::gio::DBusProxy> {
    let proxy = gtk::gio::DBusProxy::for_bus_sync(
        gtk::gio::BusType::System,
        gtk::gio::DBusProxyFlags::NONE,
        None,
        "net.hadess.SensorProxy",
        "/net/hadess/SensorProxy",
        "net.hadess.SensorProxy",
        None::<&gtk::gio::Cancellable>,
    )
    .ok()?;
    proxy
        .call_sync("ClaimLight", None, gtk::gio::DBusCallFlags::NONE, 2000, None::<&gtk::gio::Cancellable>)
        .ok()?;
    Some(proxy)
}

// Phosh's rotation lock (none without its schema)
fn rotation_lock() -> Option<gtk::gio::Settings> {
    let id = "org.gnome.settings-daemon.peripherals.touchscreen";
    gtk::gio::SettingsSchemaSource::default()?.lookup(id, true)?;
    Some(gtk::gio::Settings::new(id))
}

// the display's transform (wlr-randr's "Transform:"; the L16's panel, DSI-1), and setting
// it, as light-lfc-rotate does: 270 is landscape, the camera held as a camera
fn display_transform() -> Option<String> {
    let out = Command::new("wlr-randr").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|l| l.trim().strip_prefix("Transform:").map(|t| t.trim().to_string()))
}

fn set_display_transform(t: &str) {
    if let Err(e) = Command::new("wlr-randr").args(["--output", "DSI-1", "--transform", t]).status() {
        eprintln!("nebula: display transform: {e}");
    }
}

// the screen off, as the power button turns it off (Phosh's screensaver)
fn blank_screen() {
    let Ok(bus) = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, None::<&gtk::gio::Cancellable>) else {
        return;
    };
    bus.call(
        Some("org.gnome.ScreenSaver"),
        "/org/gnome/ScreenSaver",
        "org.gnome.ScreenSaver",
        "SetActive",
        Some(&(true,).to_variant()),
        None,
        gtk::gio::DBusCallFlags::NONE,
        -1,
        None::<&gtk::gio::Cancellable>,
        |r| {
            if let Err(e) = r {
                eprintln!("nebula: blanking the screen: {e}");
            }
        },
    );
}

// iio-sensor-proxy's accelerometer orientation, claimed for as long as the app runs
fn accel_proxy() -> Option<gtk::gio::DBusProxy> {
    let proxy = gtk::gio::DBusProxy::for_bus_sync(
        gtk::gio::BusType::System,
        gtk::gio::DBusProxyFlags::NONE,
        None,
        "net.hadess.SensorProxy",
        "/net/hadess/SensorProxy",
        "net.hadess.SensorProxy",
        None::<&gtk::gio::Cancellable>,
    )
    .ok()?;
    proxy
        .call_sync("ClaimAccelerometer", None, gtk::gio::DBusCallFlags::NONE, 2000, None::<&gtk::gio::Cancellable>)
        .ok()?;
    Some(proxy)
}

// the UI's quarter turns for iio-sensor-proxy's orientation (relative to the panel, whose
// own upright is portrait): held as a camera, right-up (or left-up, upside down: stock
// doesn't turn for it); portrait with the shutter down, normal (stock's PORTRAIT, -90);
// shutter up, bottom-up (PORTRAIT_REVERSE, +90). None: lying flat, unknown (keep the last)
fn quarter_for(orientation: &str) -> Option<i32> {
    match orientation {
        "right-up" | "left-up" => Some(0),
        "normal" => Some(-1),
        "bottom-up" => Some(1),
        _ => None,
    }
}

// the camera modules' temperature (whole degrees C): the light-ccb driver's hwmon, from
// ASIC1's telemetry; none without a recent reading
fn camera_temp() -> Option<i32> {
    for e in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        if std::fs::read_to_string(e.path().join("name")).is_ok_and(|n| n.trim() == "light_ccb") {
            let m: i64 = std::fs::read_to_string(e.path().join("temp1_input")).ok()?.trim().parse().ok()?;
            return Some((m / 1000) as i32);
        }
    }
    None
}

// where photos go (as the capture's out path)
fn photos_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Pictures)
        .unwrap_or_else(|| glib::home_dir().join("Pictures"))
        .join("L16")
}

// free and total bytes where photos go
fn photos_space() -> Option<(u64, u64)> {
    let dir = photos_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = std::ffi::CString::new(dir.to_string_lossy().as_bytes()).ok()?;
    let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut fs) } != 0 {
        return None;
    }
    let block = fs.f_frsize as u64;
    Some((fs.f_bavail as u64 * block, fs.f_blocks as u64 * block))
}

fn photos_free() -> Option<u64> {
    photos_space().map(|(free, _)| free)
}

// a photo's size: the mean of the last ten LRIs, or stock's 180 MiB
fn photo_size() -> u64 {
    let mut v: Vec<(std::time::SystemTime, u64)> = std::fs::read_dir(photos_dir())
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "lri"))
                .filter_map(|e| e.metadata().ok())
                .filter_map(|m| Some((m.modified().ok()?, m.len())))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.truncate(10);
    if v.is_empty() {
        return 180 << 20;
    }
    (v.iter().map(|x| x.1).sum::<u64>() / v.len() as u64).max(1 << 20)
}

// the focus marks' layer (room for the 10% growth, the 25 px shake and the lock above)
const FOCUS_AREA: (f64, f64) = (200.0, 220.0);

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_path();
    cr.arc(x + w - r, y + r, r, -0.5 * PI, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, 0.5 * PI);
    cr.arc(x + r, y + h - r, r, 0.5 * PI, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn build(gapp: &gtk::Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css(0));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = gtk::ApplicationWindow::builder().application(gapp).title("Nebula").build();
    window.add_css_class("camera");

    let (pipeline, paintable) = make_pipeline();
    let bus = pipeline
        .bus()
        .expect("bus")
        .add_watch_local(|_, msg| {
            if let gst::MessageView::Error(e) = msg.view() {
                eprintln!("nebula: preview: {} ({:?})", e.error(), e.debug());
            }
            glib::ControlFlow::Continue
        })
        .expect("bus watch");

    // preview, with the grid and focus marks over it
    let view = ZoomView::new(&paintable);
    view.set_hexpand(true);
    view.set_vexpand(true);
    // the grid (only while it is on) and the histogram, as textures (canvas.rs)
    let marks = Canvas::new();
    marks.set_can_target(false);
    marks.set_visible(false);
    let hist_area = Canvas::new();
    hist_area.set_size_request(208, 88);
    hist_area.set_halign(gtk::Align::Start);
    hist_area.set_valign(gtk::Align::Start);
    hist_area.set_can_target(false);
    hist_area.set_visible(false);
    // what turns for portrait, re-laid out (rotate.rs)
    let mut rotators: Vec<Rotator> = Vec::new();
    let mut turn = |w: &gtk::Widget| {
        let r = Rotator::wrap(w);
        rotators.push(r.clone());
        r
    };
    let hist_turn = turn(hist_area.upcast_ref());
    // the focus marks: a small layer of their own at the focus point (they animate, and a
    // full-screen Cairo layer redrawn every frame cost most of a core)
    let focus_area = gtk::DrawingArea::new();
    focus_area.set_size_request(FOCUS_AREA.0 as i32, FOCUS_AREA.1 as i32);
    focus_area.set_halign(gtk::Align::Start);
    focus_area.set_valign(gtk::Align::Start);
    focus_area.set_can_target(false);
    focus_area.set_visible(false);
    let preview = gtk::Overlay::new();
    preview.set_child(Some(&view));
    preview.add_overlay(&marks);
    preview.add_overlay(&hist_turn);
    preview.add_overlay(&focus_area);
    let blackout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    blackout.add_css_class("blackout");
    blackout.set_can_target(false);
    blackout.set_visible(false);
    preview.add_overlay(&blackout);
    let frame = gtk::AspectFrame::new(0.5, 0.5, 4.0 / 3.0, false);
    frame.set_child(Some(&preview));
    frame.set_hexpand(true);

    // left: the three encoders, each square so that turning it for portrait keeps the column
    let encoders: Vec<Canvas> = (0..3)
        .map(|_| {
            let c = Canvas::new();
            c.set_size_request(132, 132);
            c.add_css_class("encoder");
            c
        })
        .collect();
    let flyout = Canvas::new();
    flyout.set_size_request(FLYOUT.0, 84);
    let wheels = Ruler::new();
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("flyout");
    card.append(&flyout);
    card.append(&wheels);
    let flyout_turn = turn(card.upcast_ref());
    flyout_turn.set_halign(gtk::Align::Start);
    flyout_turn.set_valign(gtk::Align::Start);
    flyout_turn.set_can_target(false);
    flyout_turn.add_css_class("fade");
    flyout_turn.add_css_class("off");
    let left = gtk::Box::new(gtk::Orientation::Vertical, 10);
    left.set_valign(gtk::Align::Center);
    left.set_margin_start(26);
    left.set_margin_end(8);
    left.set_margin_top(36);
    let enc_turn: Vec<Rotator> = encoders.iter().map(|c| turn(c.upcast_ref())).collect();
    for t in &enc_turn {
        t.add_css_class("enc-wrap");
        left.append(t);
    }

    // right: last photo, the dials around the shutter, the toolbar opener
    let thumb = gtk::Image::new();
    thumb.set_pixel_size(72);
    thumb.add_css_class("thumb");
    let thumb_spin = gtk::DrawingArea::new();
    thumb_spin.set_can_target(false);
    let thumb_box = gtk::Overlay::new();
    thumb_box.set_child(Some(&thumb));
    thumb_box.add_overlay(&thumb_spin);
    thumb_box.set_halign(gtk::Align::Center);
    thumb_box.set_margin_top(16);
    // not turned for portrait: it is the preview's frame, upright as the preview is
    // the gallery, at the newest photo
    let open_gallery = gtk::GestureClick::new();
    open_gallery.connect_released(|_, _, _, _| {
        let dir = glib::user_special_dir(glib::UserDirectory::Pictures)
            .unwrap_or_else(|| glib::home_dir().join("Pictures"))
            .join("L16");
        let newest = std::fs::read_dir(dir).ok().and_then(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "lri"))
                .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
                .max()
                .map(|(_, p)| gtk::gio::File::for_path(p))
        });
        #[cfg(target_os = "linux")]
        {
            let Some(app) = gtk::gio::DesktopAppInfo::new("org.l16linux.Gallery.desktop") else { return };
            let ctx = gdk::Display::default().map(|d| d.app_launch_context());
            if let Err(e) = app.launch(&newest.into_iter().collect::<Vec<_>>(), ctx.as_ref()) {
                eprintln!("launching the gallery: {e}");
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = newest;
    });
    thumb_box.add_controller(open_gallery);
    let dial = |size: i32| {
        let d = Canvas::new();
        d.set_size_request(size, size);
        d.set_halign(gtk::Align::Center);
        d
    };
    let shutter = dial(92);
    shutter.add_css_class("spin");
    // right: settings and close, the mode keys, the grid of keys, the last photo and the shutter
    let key_grid = gtk::Grid::new();
    key_grid.set_row_spacing(8);
    key_grid.set_column_spacing(8);
    key_grid.set_row_homogeneous(true);
    key_grid.set_column_homogeneous(true);
    let mode_btn = icons::button(icons::MODES[0], "");
    let timer_btn = icons::button(icons::TIMER_OFF, "");
    let grid_btn = icons::button(icons::GRID_OFF, "");
    let hist_btn = icons::button(icons::HISTOGRAM, "");
    let burst_btn = icons::button(icons::BURST, "");
    let flash_btn = icons::button(icons::FLASH_OFF, "");
    let wb_btn = icons::button(icons::WB[0], "");
    let afd_btn = icons::button(icons::FOCUS_AUTO, "");
    let assist_btn = icons::button(icons::ASSIST_OFF, "");
    let meter_btn = icons::button(icons::METER[0], "");
    let geo_btn = icons::button(icons::GEO_OFF, "");
    let strip_btn = icons::button(icons::STRIP, "");
    let mut keys: Vec<gtk::Button> = Vec::new();
    for (k, b) in [
        &mode_btn, &flash_btn, &wb_btn, &timer_btn, &grid_btn, &hist_btn, &assist_btn, &burst_btn, &afd_btn, &meter_btn,
        &geo_btn, &strip_btn,
    ]
    .into_iter()
    .enumerate()
    {
        keys.push(b.clone());
        b.add_css_class("key");
        b.add_css_class("spin");
        key_grid.attach(b, (k % 3) as i32, (k / 3) as i32, 1, 1);
    }
    // the sidebar (a swipe in from the left edge): settings and close, the quick checkboxes, and
    // the About lines at the foot, over a dimmed viewfinder
    let side_row = |icon: char, name: &str| {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        line.append(&icons::label(icon));
        let l = gtk::Label::new(Some(name));
        l.set_xalign(0.0);
        line.append(&l);
        let b = gtk::Button::new();
        b.set_child(Some(&line));
        b.add_css_class("side-row");
        b
    };
    let settings_btn = side_row(icons::COG, "Settings");
    let close_btn = side_row(icons::CLOSE, "Close the camera");
    let side_title = gtk::Label::new(Some("NEBULA"));
    side_title.add_css_class("side-title");
    side_title.set_xalign(0.0);
    let side_head = gtk::Label::new(Some("QUICK SETTINGS"));
    side_head.add_css_class("side-head");
    side_head.set_xalign(0.0);
    let side_quick = gtk::Box::new(gtk::Orientation::Vertical, 14);
    side_quick.add_css_class("side-quick");
    let side_about = gtk::Label::new(None);
    side_about.add_css_class("side-about");
    side_about.set_xalign(0.0);
    let side_fill = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side_fill.set_vexpand(true);
    let side_panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    side_panel.add_css_class("sidebar");
    side_panel.add_css_class("side-hidden");
    side_panel.set_size_request(320, -1);
    side_panel.set_halign(gtk::Align::Start);
    side_panel.set_valign(gtk::Align::Fill);
    side_panel.set_can_target(false);
    for w in [side_title.upcast_ref::<gtk::Widget>(), settings_btn.upcast_ref(), close_btn.upcast_ref(), side_head.upcast_ref(), side_quick.upcast_ref(), side_fill.upcast_ref(), side_about.upcast_ref()] {
        side_panel.append(w);
    }
    let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
    scrim.add_css_class("scrim");
    scrim.add_css_class("fade");
    scrim.add_css_class("off");
    scrim.set_can_target(false);
    // the mode picker: a card of rows, each the mode's icon and full name
    let picker_card = gtk::Box::new(gtk::Orientation::Vertical, 2);
    picker_card.add_css_class("picker");
    let picker_rows: Vec<gtk::Button> = MODE_NAMES
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 14);
            line.append(&icons::label(icons::MODES[i]));
            let l = gtk::Label::new(Some(name));
            l.set_xalign(0.0);
            line.append(&l);
            let b = gtk::Button::new();
            b.set_child(Some(&line));
            b.add_css_class("pick-row");
            picker_card.append(&b);
            b
        })
        .collect();
    let picker_turn = turn(picker_card.upcast_ref());
    picker_turn.set_halign(gtk::Align::Start);
    picker_turn.set_valign(gtk::Align::Start);
    picker_turn.add_css_class("fade");
    picker_turn.add_css_class("off");
    picker_turn.set_can_target(false);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 10);
    right.set_size_request(RIGHT_W as i32, -1);
    right.set_halign(gtk::Align::End);
    right.set_valign(gtk::Align::Fill);
    right.set_margin_end(10);
    right.set_margin_top(10);
    right.set_margin_bottom(10);
    let spacer = || {
        let s = gtk::Box::new(gtk::Orientation::Vertical, 0);
        s.set_vexpand(true);
        s
    };
    thumb_box.set_margin_top(0);
    thumb_box.set_valign(gtk::Align::End);
    let shutter_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let shutter_fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    shutter_fill.set_hexpand(true);
    shutter_row.append(&thumb_box);
    shutter_row.append(&shutter_fill);
    shutter_row.append(&shutter);
    // the pinned keys, together, while the controls are stowed
    let pin_strip = gtk::Box::new(gtk::Orientation::Vertical, 8);
    pin_strip.set_halign(gtk::Align::Center);
    pin_strip.set_visible(false);
    right.append(&key_grid);
    right.append(&pin_strip);
    right.append(&spacer());
    right.append(&shutter_row);

    // the preview with the lens strip under it
    let centre = gtk::Overlay::new();
    centre.set_hexpand(true);
    centre.set_margin_end(RESERVE_FULL as i32);
    frame.set_vexpand(true);
    frame.set_margin_bottom(LENS_ROOM as i32);
    centre.set_child(Some(&frame));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&left);
    row.append(&centre);
    // the keys float over the preview's right edge, so the preview can take their room
    let deck = gtk::Overlay::new();
    deck.set_child(Some(&row));
    deck.add_overlay(&right);

    // the settings screen (OpenLight's: a list of title, explanation and value)
    let settings_nav = gtk::ListBox::new();
    settings_nav.add_css_class("nav");
    settings_nav.set_selection_mode(gtk::SelectionMode::Single);
    let nav_scroll = gtk::ScrolledWindow::new();
    nav_scroll.set_child(Some(&settings_nav));
    nav_scroll.set_size_request(250, -1);
    nav_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    let settings_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    settings_pane.add_css_class("pane");
    let pane_scroll = gtk::ScrolledWindow::new();
    pane_scroll.set_child(Some(&settings_pane));
    pane_scroll.set_hexpand(true);
    pane_scroll.set_vexpand(true);
    pane_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    let settings_back = icons::button(icons::ARROW_LEFT, "settings");
    settings_back.add_css_class("flat-white");
    settings_back.set_halign(gtk::Align::Start);
    settings_back.set_margin_start(16);
    let settings_body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    settings_body.append(&nav_scroll);
    settings_body.append(&pane_scroll);
    let settings_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    settings_box.add_css_class("settings");
    settings_box.append(&settings_back);
    settings_box.append(&settings_body);
    let settings_page = gtk::Overlay::new();
    settings_page.set_child(Some(&turn(settings_box.upcast_ref())));
    settings_page.set_visible(false);
    settings_page.add_css_class("page");
    settings_page.add_css_class("page-off");
    // the notices at the top of the screen: a small pill, and under it an alert with a line of text
    let pill_label = gtk::Label::new(None);
    pill_label.add_css_class("pill");
    pill_label.set_halign(gtk::Align::Center);
    pill_label.set_valign(gtk::Align::Start);
    pill_label.set_margin_top(10);
    let pill_turn = turn(pill_label.upcast_ref());
    pill_turn.set_can_target(false);
    pill_turn.add_css_class("fade");
    pill_turn.add_css_class("off");
    let alert_icon = gtk::Label::new(None);
    alert_icon.add_css_class("alert-icon");
    let alert_title = gtk::Label::new(None);
    alert_title.add_css_class("bubble-title");
    alert_title.set_xalign(0.0);
    let alert_text = gtk::Label::new(None);
    alert_text.add_css_class("bubble-text");
    alert_text.set_xalign(0.0);
    alert_text.set_wrap(true);
    alert_text.set_max_width_chars(34);
    let alert_body = gtk::Box::new(gtk::Orientation::Vertical, 3);
    alert_body.append(&alert_title);
    alert_body.append(&alert_text);
    // the camera's back with the covered lenses lit (for the lens-blocked alert only)
    let lens_art = gtk::DrawingArea::new();
    lens_art.set_size_request(208, 116);
    lens_art.set_visible(false);
    let alert_card = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    alert_card.add_css_class("bubble");
    alert_card.append(&lens_art);
    alert_card.append(&alert_icon);
    alert_card.append(&alert_body);
    alert_card.set_halign(gtk::Align::Center);
    alert_card.set_valign(gtk::Align::Start);
    alert_card.set_margin_top(10);
    let alert_turn = turn(alert_card.upcast_ref());
    alert_turn.set_can_target(false);
    alert_turn.add_css_class("fade");
    alert_turn.add_css_class("off");
    let zoom_chips: Vec<gtk::Button> = PRIMES
        .iter()
        .map(|p| {
            let b = gtk::Button::with_label(&format!("{p:.0}"));
            b.add_css_class("zoom-chip");
            if let Some(l) = b.child() {
                l.add_css_class("spin");
            }
            b
        })
        .collect();
    let zoom_pill = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    zoom_pill.add_css_class("zoom-pill");
    for c in &zoom_chips {
        zoom_pill.append(c);
    }
    zoom_pill.set_halign(gtk::Align::Center);
    zoom_pill.set_valign(gtk::Align::End);
    zoom_pill.set_margin_bottom(10);
    centre.add_overlay(&zoom_pill);
    let countdown = gtk::Label::new(None);
    countdown.add_css_class("countdown");
    countdown.add_css_class("spin");
    countdown.set_visible(false);
    countdown.set_can_target(false);

    // OpenLight's BurstView: black over everything (touches too), the frame number, then
    // "saving captures" under three dots until the photo is taken
    let burst_label = gtk::Label::new(None);
    burst_label.add_css_class("burst-count");
    let burst_dots = gtk::DrawingArea::new();
    burst_dots.set_size_request(72, 72);
    burst_dots.set_halign(gtk::Align::Center);
    let saving_text = gtk::Label::new(Some("saving captures"));
    saving_text.add_css_class("burst-saving");
    let burst_saving = gtk::Box::new(gtk::Orientation::Vertical, 24);
    burst_saving.append(&burst_dots);
    burst_saving.append(&saving_text);
    burst_saving.set_visible(false);
    let burst_inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
    burst_inner.set_valign(gtk::Align::Center);
    burst_inner.set_halign(gtk::Align::Center);
    burst_inner.append(&burst_label);
    burst_inner.append(&burst_saving);
    let burst_screen = gtk::Box::new(gtk::Orientation::Vertical, 0);
    burst_screen.add_css_class("burst-screen");
    burst_inner.set_vexpand(true);
    burst_screen.append(&turn(burst_inner.upcast_ref()));
    burst_screen.set_visible(false);
    burst_screen.add_controller(gtk::GestureClick::new()); // swallows taps
    let burst_badge = gtk::Label::new(None);
    burst_badge.add_css_class("burst-badge");
    burst_badge.set_halign(gtk::Align::Center);
    burst_badge.set_visible(false);
    burst_badge.add_css_class("spin");
    left.prepend(&burst_badge);
    let tripod_badge = icons::label(icons::CAMERA_LOCK);
    tripod_badge.add_css_class("assist-badge");
    tripod_badge.set_halign(gtk::Align::Center);
    tripod_badge.set_visible(false);
    tripod_badge.add_css_class("spin");
    left.prepend(&tripod_badge);
    let shake_badge = icons::label(icons::HAND_WAVE);
    shake_badge.add_css_class("assist-badge");
    shake_badge.set_halign(gtk::Align::Center);
    shake_badge.set_visible(false);
    shake_badge.add_css_class("spin");
    left.prepend(&shake_badge);
    let moon_badge = icons::label(icons::MOON);
    moon_badge.add_css_class("assist-badge");
    moon_badge.set_halign(gtk::Align::Center);
    moon_badge.set_visible(false);
    moon_badge.add_css_class("spin");
    left.prepend(&moon_badge);

    // stock's lens-blocked warning: the camera's back with the covered sensors, top centre

    // stock's device status: captures left and the battery, top left
    let storage_label = gtk::Label::new(None);
    let battery_label = gtk::Label::new(None);
    storage_label.set_halign(gtk::Align::Start);
    battery_label.set_halign(gtk::Align::Start);
    let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 20);
    status_box.add_css_class("device-status");
    status_box.append(&storage_label);
    status_box.append(&battery_label);
    status_box.set_halign(gtk::Align::Start);
    status_box.set_valign(gtk::Align::Start);
    status_box.set_margin_start(12);
    status_box.set_margin_top(16);
    status_box.set_can_target(false);
    storage_label.add_css_class("spin");
    battery_label.add_css_class("spin");
    // stock's LowBatteryFragment: over everything (touches too) at 10% or less, until 12%
    let battery_screen = gtk::Box::new(gtk::Orientation::Vertical, 16);
    battery_screen.add_css_class("battery-screen");
    let battery_icon = icons::label(icons::BATTERY_ALERT);
    battery_icon.set_markup(&format!(
        "<span font_family=\"{}\" size=\"400%\">{}</span>",
        icons::FAMILY,
        icons::BATTERY_ALERT
    ));
    let battery_inner = gtk::Box::new(gtk::Orientation::Vertical, 16);
    battery_inner.set_valign(gtk::Align::Center);
    battery_inner.set_vexpand(true);
    battery_inner.append(&battery_icon);
    battery_inner.append(&gtk::Label::new(Some("battery low")));
    battery_screen.append(&turn(battery_inner.upcast_ref()));
    battery_screen.set_visible(false);
    battery_screen.add_controller(gtk::GestureClick::new()); // swallows taps

    // stock's overheating warning (55 C) and its cool-off screen (65 C, ThermalShutdownFragment)
    let thermal_warning = gtk::Label::new(None);
    thermal_warning.set_markup(&icons::markup(icons::THERMOMETER, "Overheating! Shut down your camera to cool off."));
    thermal_warning.add_css_class("thermal-warning");
    thermal_warning.set_halign(gtk::Align::Center);
    thermal_warning.set_valign(gtk::Align::End);
    thermal_warning.set_margin_bottom(8);
    let thermal_turn = turn(thermal_warning.upcast_ref());
    thermal_turn.set_can_target(false);
    thermal_turn.add_css_class("fade");
    thermal_turn.add_css_class("off");
    let hot_screen = gtk::Box::new(gtk::Orientation::Vertical, 16);
    hot_screen.add_css_class("battery-screen");
    let hot_icon = gtk::Label::new(None);
    hot_icon.set_markup(&format!("<span font_family=\"{}\" size=\"400%\">{}</span>", icons::FAMILY, icons::THERMOMETER));
    let hot_text = gtk::Label::new(Some("Your camera is getting a little too hot.\nPlease wait for it to cool off."));
    hot_text.set_justify(gtk::Justification::Center);
    let hot_inner = gtk::Box::new(gtk::Orientation::Vertical, 16);
    hot_inner.set_valign(gtk::Align::Center);
    hot_inner.set_vexpand(true);
    hot_inner.append(&hot_icon);
    hot_inner.append(&hot_text);
    hot_screen.append(&turn(hot_inner.upcast_ref()));
    hot_screen.set_visible(false);
    hot_screen.add_controller(gtk::GestureClick::new()); // swallows taps

    let root = gtk::Overlay::new();
    root.set_child(Some(&deck));
    // on the preview, at its bottom edge as the camera is held (place_status)
    centre.add_overlay(&thermal_turn);
    root.add_overlay(&status_box);
    preview.add_overlay(&pill_turn);
    preview.add_overlay(&alert_turn);
    root.add_overlay(&countdown);
    root.add_overlay(&burst_screen);
    root.add_overlay(&flyout_turn);
    root.add_overlay(&scrim);
    root.add_overlay(&side_panel);
    root.add_overlay(&picker_turn);
    root.add_overlay(&battery_screen);
    root.add_overlay(&hot_screen);
    root.add_overlay(&settings_page);
    window.set_child(Some(&root));

    let (stage_tx, stage_rx) = mpsc::channel();
    // control writes that can wait for the driver (an AF run holds it for seconds)
    let (ctl_tx, ctl_rx) = mpsc::channel::<(u32, i32)>();
    thread::spawn(move || {
        let c = ccb::Ccb::open();
        while let Ok((id, v)) = ctl_rx.recv() {
            if let Some(c) = &c {
                c.set(id, v);
            }
        }
    });
    let (input_tx, input_rx) = mpsc::channel();
    input::spawn(input_tx);
    // the metered exposure, off the UI thread (reading a control waits for the driver, which
    // a focus run holds for seconds: the UI stalled and drags were dropped)
    let metered: Arc<[std::sync::atomic::AtomicI32; 4]> = Arc::new(Default::default());
    let m = metered.clone();
    thread::spawn(move || {
        let Some(c) = ccb::Ccb::open() else { return };
        loop {
            m[0].store(c.get(ccb::AE_ISO).unwrap_or(0), Ordering::Relaxed);
            m[1].store(c.get(ccb::AE_EXPOSURE_US).unwrap_or(0), Ordering::Relaxed);
            m[2].store(c.get(ccb::PREVIEW_BOOST).unwrap_or(100), Ordering::Relaxed);
            m[3].store(c.get(ccb::STACKED).unwrap_or(0), Ordering::Relaxed);
            thread::sleep(Duration::from_millis(300));
        }
    });
    let (gyro_on, still) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)));
    let focal = Arc::new(AtomicU32::new(280));
    let moved = Arc::new(AtomicBool::new(false));
    gyro::spawn(gyro_on.clone(), still.clone(), focal.clone(), moved.clone());
    // the lens-blocked sensors, read while the preview runs (as the gyro)
    let blocked = Arc::new(AtomicU8::new(0));
    prox::spawn(gyro_on.clone(), blocked.clone());
    let app = Rc::new(App {
        st: RefCell::new(State {
            mode: Mode::Auto,
            iso: 1.0,
            ev: 0.5,
            shutter: secs_pos(1.0 / 60.0),
            zoom: ZOOM_MIN,
            module: 0,
            timer: 0,
            grid: 0,
            histogram: false,
            assist: 0,
            accent: 0,
            contrast: 0,
            sparkle: true,
            pinned: 1,
            strip_fn: 0,
            strip_set: 0b1111,
            strip_tap_at: None,
            busy: false,
            counting: false,
            saving: 0,
            seq: 0,
            burst_count: 0,
            burst_captured: false,
            burst: 0,
            flash: 0,
            wb: 0,
            dragged: false,
            wheel: None,
            wheel_start: 0.0,
            wheel_close: None,
            haptics: 1,
            continuous: false,
            zoom_start: ZOOM_MIN,
            zoom_raw: ZOOM_MIN,
            zoom_wheel_until: None,
            focus_until: None,
            focus_t0: None,
            focus_done: None,
            focus_at: None,
            zoom_sent: Instant::now(),
            metering: 1, // stock's default: touch-weighted
            caf: true,
            tools: TOOLS.to_vec(),
            tool_cycle: false,
            caf_zoom: None,
            caf_pause_until: None,
            stacked: true,
            exposure_info: true,
            inverse_wheel: false,
            strip_zoom: true,
            asleep: false,
            unseen_since: None,
            screen_off: false,
            fast_loop_on: false,
            tripod: false,
            polls: 0,
            lens_warning: 2,
            lens_mask: 0,
            device_status: true,
            battery: (100, false),
            battery_low: false,
            captures_left: 0,
            storage_warned: 0,
            pocket: true,
            pocket_since: None,
            geotag: false,
            thermal: 0,
            thermal_pause_until: None,
            live_iso: 0,
            live_secs: 0.0,
            strip_down: false,
            strip_active: false,
            strip_lock: 0,
            strip_slid_at: None,
            strip_x0: 0,
            strip_x: 0,
            strip_t0: Instant::now(),
            settle: None,
            switching: None,
        }),
        ccb: ccb::Ccb::open(),
        focusing: Arc::new(AtomicBool::new(false)),
        stage_tx,
        stage_rx,
        input_rx,
        ctl_tx,
        metered: metered.clone(),
        gyro_on: gyro_on.clone(),
        focal,
        still: still.clone(),
        moved: moved.clone(),
        blocked: blocked.clone(),
        status_box,
        light: light_proxy(),
        accel: accel_proxy(),
        quarter: Cell::new(0),
        pill_turn: pill_turn.clone(),
        pill_label: pill_label.clone(),
        pill_timer: RefCell::new(None),
        alert_turn: alert_turn.clone(),
        alert_icon: alert_icon.clone(),
        lens_art: lens_art.clone(),
        alert_title: alert_title.clone(),
        alert_text: alert_text.clone(),
        alert_timer: RefCell::new(None),
        alert_key: Cell::new(""),
        zoom_pill: zoom_pill.clone(),
        thermal_t: Cell::new(0.0),
        thermal_anim: RefCell::new(None),
        thermal_turn: thermal_turn.clone(),
        zoom_chips: zoom_chips.clone(),
        idle_cookie: Cell::new(0),
        landscape_held: RefCell::new(None),
        rotators,
        geo: RefCell::new(geo::Geo::default()),
        storage_label,
        battery_label,
        battery_screen,
        hot_screen,
        transfers: RefCell::new(None),
        transfers_wait: RefCell::new(None),
        transfer_turn: Arc::new(Mutex::new(())),
        pipeline,
        paintable,
        _bus: bus,
        view,
        marks,
        hist_area,
        focus_area,
        wheels,
        encoders,
        enc_shown: Cell::new([0; 3]),
        flyout: flyout.clone(),
        flyout_turn: flyout_turn.clone(),
        enc_roll: (0..3).map(|_| Roll::new()).collect(),
        flyout_roll: Roll::new(),
        flyout_cells: Cell::new(4),
        bright: Cell::new(false),
        shutter_flash: Cell::new(None),
        flyout_unit: Cell::new(""),
        flyout_suffix: Cell::new(""),
        flyout_at: Cell::new(255),
        css: provider.clone(),
        root: root.clone(),
        shutter,
        thumb,
        thumb_spin,
        blackout,
        burst_screen,
        burst_label,
        burst_saving,
        burst_dots,
        burst_badge,
        tripod_badge,
        af_outcome: Arc::new(std::sync::atomic::AtomicI32::new(0)),
        marks_ticking: Cell::new(false),
        focus_centre: Cell::new((0.0, 0.0)),
        marks_grid: Cell::new(0),
        dials_shown: Cell::new(None),
        moon_badge,
        shake_badge,
        preview_gain: Cell::new(1.0),
        mode_btn,
        keys,
        key_grid: key_grid.clone(),
        pin_strip: pin_strip.clone(),
        deck: deck.clone(),
        centre: centre.clone(),
        frame: frame.clone(),
        right: right.clone(),
        shutter_row: shutter_row.clone(),
        shutter_fill: shutter_fill.clone(),
        stow_t: Cell::new(0.0),
        stow_anim: RefCell::new(None),
        compact: Cell::new(false),
        stowed: Cell::new(false),
        side_panel: side_panel.clone(),
        scrim: scrim.clone(),
        side_quick: side_quick.clone(),
        side_about: side_about.clone(),
        picker_turn,
        picker_card,
        picker_rows,
        picker_timer: RefCell::new(None),
        enc_turn,
        hint_at: RefCell::new(HashMap::new()),
        meter_btn,
        geo_btn,
        strip_btn,
        timer_btn,
        grid_btn,
        hist_btn,
        hist: RefCell::new(vec![0; HIST_BINS * 4]),
        burst_btn,
        flash_btn,
        wb_btn,
        afd_btn,
        assist_btn,
        settings_nav: settings_nav.clone(),
        settings_pane: settings_pane.clone(),
        cal: wb::Calibration::load(),
        motor: haptics::Haptics::open(),
        photo_args: RefCell::new(HashMap::new()),
        settings_page,
        last_saved: RefCell::new(String::new()),
        sleep_inhibitor: RefCell::new(None),
        countdown,
    });
    // portrait: follow the accelerometer's orientation
    if let Some(accel) = &app.accel {
        let a = app.clone();
        accel.connect_local("g-properties-changed", false, move |v| {
            if let Some(p) = v.first().and_then(|p| p.get::<gtk::gio::DBusProxy>().ok()) {
                a.follow_orientation(&p);
            }
            None
        });
        app.follow_orientation(accel);
    }
    {
        let mut st = app.st.borrow_mut();
        st.load(&settings::load());
        *app.last_saved.borrow_mut() = st.saved();
    }
    if app.ccb.is_none() {
        if !dev::demo() {
            app.show_status("no light-ccb camera driver", 0);
        }
    }

    // drawing
    let a = app.clone();
    app.marks.set_draw_func(move |_, cr, w, h| a.draw_marks(cr, w as f64, h as f64));
    let a = app.clone();
    app.hist_area.set_draw_func(move |_, cr, _, _| a.draw_histogram(cr));
    let a = app.clone();
    app.focus_area.set_draw_func(move |_, cr, _, _| {
        let st = a.st.borrow();
        if st.focus_until.is_some_and(|t| Instant::now() < t) {
            a.draw_focus(cr, &st, a.focus_centre.get());
        }
    });
    let a = app.clone();
    app.lens_art.set_draw_func(move |_, cr, w, h| a.draw_lens_art(cr, w as f64, h as f64));
    let a = app.clone();
    app.shutter.set_draw_func(move |_, cr, w, h| a.draw_shutter(cr, w as f64, h as f64));
    for (c, dial) in app.encoders.iter().zip([Dial::Iso, Dial::Shutter, Dial::Ev]) {
        let a = app.clone();
        c.set_draw_func(move |_, cr, w, h| a.draw_encoder(cr, w as f64, h as f64, dial));
    }
    let a = app.clone();
    app.flyout.set_draw_func(move |_, cr, w, h| a.draw_flyout(cr, w as f64, h as f64));
    let a = app.clone();
    app.thumb_spin.set_draw_func(move |_, cr, w, h| a.draw_thumb_spin(cr, w as f64, h as f64));
    app.burst_dots.set_draw_func(|_, cr, w, h| {
        // three dots going round
        let t = glib::monotonic_time() as f64 / 1e6;
        let (w, h) = (w as f64, h as f64);
        for i in 0..3 {
            let a = t * 4.0 + i as f64 * 2.0 * PI / 3.0;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.arc(w / 2.0 + 20.0 * a.cos(), h / 2.0 + 20.0 * a.sin(), 5.0, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
    });

    // preview: tap focuses (or closes the toolbar), drag and pinch zoom
    let click = gtk::GestureClick::new();
    let a = app.clone();
    click.connect_released(move |_, _, x, y| {
        if a.st.borrow().dragged {
            return;
        }
        if a.picker_open() {
            a.close_picker();
            return;
        }
        if !a.side_panel.has_css_class("side-hidden") {
            a.show_sidebar(false);
            return;
        }
        a.focus(Some((x, y)));
    });
    app.view.add_controller(click);
    let drag = gtk::GestureDrag::new();
    let a = app.clone();
    drag.connect_drag_begin(move |_, _, _| {
        let mut st = a.st.borrow_mut();
        st.zoom_start = st.zoom;
        st.dragged = false;
    });
    let a = app.clone();
    drag.connect_drag_update(move |_, _, dy| {
        if dy.abs() > 8.0 {
            a.st.borrow_mut().dragged = true;
            let z = a.st.borrow().zoom_start;
            a.zoom_gesture(z * 2.3f64.powf(-dy / 400.0));
        }
    });
    app.view.add_controller(drag);
    let pinch = gtk::GestureZoom::new();
    let a = app.clone();
    pinch.connect_begin(move |_, _| {
        let mut st = a.st.borrow_mut();
        st.zoom_start = st.zoom;
        st.dragged = true;
    });
    let a = app.clone();
    pinch.connect_scale_changed(move |_, s| {
        let z = a.st.borrow().zoom_start;
        a.zoom_gesture(z * s);
    });
    app.view.add_controller(pinch);

    // the encoders: a touch grabs the value (when the mode has it in hand), a drag up raises it
    // and down lowers it; the ruler shows along the preview's edge. The encoders sit in
    // Rotators, so the drag is in the camera's own up and down in portrait too
    for (c, dial) in app.encoders.iter().zip([Dial::Iso, Dial::Shutter, Dial::Ev]) {
        let drag = gtk::GestureDrag::new();
        let a = app.clone();
        drag.connect_drag_begin(move |_, _, _| {
            if a.dial_active(dial) {
                a.wheel_grab(dial);
            }
        });
        let a = app.clone();
        drag.connect_drag_update(move |_, _, dy| a.wheel_drag(dy));
        let a = app.clone();
        drag.connect_drag_end(move |_, _, _| a.wheel_release());
        c.add_controller(drag);
    }
    let click = gtk::GestureClick::new();
    let a = app.clone();
    click.connect_released(move |_, _, _, _| a.shutter_pressed());
    app.shutter.add_controller(click);

    // swipe right on the controls: the unpinned keys go (left: back); a long press pins a key
    let swipe = gtk::GestureDrag::new();
    swipe.set_propagation_phase(gtk::PropagationPhase::Capture);
    let done = Rc::new(Cell::new(false));
    let start_x = Rc::new(Cell::new(0.0f64));
    let (d, sx) = (done.clone(), start_x.clone());
    swipe.connect_drag_begin(move |_, x, _| {
        d.set(false);
        sx.set(x);
    });
    let a = app.clone();
    swipe.connect_drag_update(move |g, dx, dy| {
        if done.get() || dx.abs() <= 56.0 || dx.abs() <= dy.abs() * 1.6 {
            return;
        }
        // away: from the controls (the right of the deck), to the right; back: to the left, anywhere
        let at_controls = start_x.get() > a.deck.width() as f64 - RIGHT_W;
        if (dx > 0.0 && at_controls && !a.stowed.get()) || (dx < 0.0 && a.stowed.get()) {
            done.set(true);
            g.set_state(gtk::EventSequenceState::Claimed);
            a.set_stowed(dx > 0.0);
        }
    });
    deck.add_controller(swipe);
    for (k, key) in app.keys.iter().enumerate() {
        let lp = gtk::GestureLongPress::new();
        let a = app.clone();
        lp.connect_pressed(move |g, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            a.toggle_pin(k);
        });
        key.add_controller(lp);
    }
    // a swipe in from the left edge: the system panel
    let edge = gtk::GestureDrag::new();
    edge.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (from_edge, shown) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    let f = from_edge.clone();
    let sh = shown.clone();
    edge.connect_drag_begin(move |_, x, _| {
        f.set(x < 24.0);
        sh.set(false);
    });
    let a = app.clone();
    edge.connect_drag_update(move |g, dx, dy| {
        if from_edge.get() && !shown.get() && dx > 44.0 && dx > dy.abs() * 1.5 {
            shown.set(true);
            g.set_state(gtk::EventSequenceState::Claimed);
            a.show_sidebar(true);
        }
    });
    root.add_controller(edge);
    // the sidebar goes with a tap on the dimmed layer, or a swipe back to the left
    let tap = gtk::GestureClick::new();
    let a = app.clone();
    tap.connect_released(move |_, _, _, _| a.show_sidebar(false));
    scrim.add_controller(tap);
    let back = gtk::GestureDrag::new();
    back.set_propagation_phase(gtk::PropagationPhase::Capture);
    let a = app.clone();
    back.connect_drag_update(move |g, dx, dy| {
        if dx < -60.0 && dx.abs() > dy.abs() * 1.5 {
            g.set_state(gtk::EventSequenceState::Claimed);
            a.show_sidebar(false);
        }
    });
    side_panel.add_controller(back);
    // (developer) SIGWINCH: a pill and an alert, to see them on the device
    {
        let a = app.clone();
        glib::unix_signal_add_local(libc::SIGWINCH, move || {
            a.show_pill(icons::FLASH_AUTO, "Flash auto", 20);
            a.show_alert("demo", icons::HAND_WAVE, "Hold steady", "The shutter is slow enough that a shaky hand will blur the photo.", 20);
            glib::ControlFlow::Continue
        });
    }
    // the mode key opens its picker; a row sets the mode. The close key closes the app as the
    // window does
    let a = app.clone();
    app.mode_btn.connect_clicked(move |_| {
        if a.picker_open() {
            a.close_picker();
        } else {
            a.open_picker();
        }
    });
    for (b, mode) in app.picker_rows.iter().zip(MODES) {
        let a = app.clone();
        b.connect_clicked(move |_| {
            a.buzz(8);
            a.set_mode(mode);
            a.close_picker();
            a.show_pill(icons::MODES[mode.index()], MODE_NAMES[mode.index()].0, 2);
        });
    }
    let w = window.clone();
    close_btn.connect_clicked(move |_| w.close());
    // the keys
    for t in TOOLS {
        let a = app.clone();
        app.tool_button(t).connect_clicked(move |_| a.tool_tap(t));
    }
    let a = app.clone();
    settings_btn.connect_clicked(move |_| {
        a.show_sidebar(false);
        a.open_settings();
    });
    for (chip, &prime) in app.zoom_chips.iter().zip(PRIMES) {
        let a = app.clone();
        chip.connect_clicked(move |_| a.zoom_to(prime));
    }
    let a = app.clone();
    settings_back.connect_clicked(move |_| a.close_settings());
    // back in front: the preview again at once
    let a = app.clone();
    window.connect_is_active_notify(move |w| {
        if w.is_active() {
            a.follow_screen();
        }
    });
    // the settings screen's categories
    let a = app.clone();
    app.settings_nav.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            settings_ui::fill_pane(&a, &a.settings_pane, row.index() as usize);
        }
    });

    // hardware: shutter button, touch strip
    let a = app.clone();
    a.fast_loop();
    let a = app.clone();
    glib::timeout_add_local(Duration::from_millis(300), move || {
        a.poll();
        glib::ControlFlow::Continue
    });

    let a = app.clone();
    // closing: the window goes at once (the shell's close animation doesn't wait for the
    // camera), then the transfer streams and the preview stop in their order and the app ends
    window.connect_close_request(move |w| {
        eprintln!("nebula: window closed");
        CLOSING.store(true, Ordering::Relaxed);
        a.release_landscape();
        w.set_visible(false);
        // the app's name on the session bus given up now: a launch while the streams stop
        // (about 3 s) was handed to this instance, which then quit, and nothing opened. Now
        // it starts a camera of its own, which waits for this one's streams (take_camera).
        if let Some(conn) = w.application().and_then(|a| a.dbus_connection()) {
            let r = conn.call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ReleaseName",
                Some(&("org.l16linux.Nebula",).to_variant()),
                None,
                gtk::gio::DBusCallFlags::NONE,
                1000,
                None::<&gtk::gio::Cancellable>,
            );
            if let Err(e) = r {
                eprintln!("nebula: giving up the app's name: {e}");
            }
        }
        let (a, w, waiting) = (a.clone(), w.clone(), a.clone());
        let finish = move || {
            let t = Instant::now();
            if let Some(t) = a.transfers.borrow_mut().as_mut() {
                t.stop();
            }
            a.stop_preview();
            a.geo.borrow_mut().stop();
            eprintln!("nebula: closed in {:.2} s", t.elapsed().as_secs_f64());
            // quit outright: started from the app grid, the application is registered on the
            // session bus and stayed running (hidden) once its window was gone
            let gapp = w.application();
            w.destroy();
            if let Some(gapp) = gapp {
                gapp.quit();
            }
        };
        // photos on their way (being taken, transferred or put together) are finished first:
        // stopping the transfer streams, or quitting under the threads, lost them (a minute
        // at most)
        let since = Instant::now();
        let mut finish = Some(finish);
        glib::timeout_add_local(Duration::from_millis(200), move || {
            let left = {
                let st = waiting.st.borrow();
                st.saving + st.busy as u32
            };
            if left > 0 && since.elapsed() < Duration::from_secs(60) {
                return glib::ControlFlow::Continue;
            }
            if left > 0 {
                eprintln!("nebula: closing with {left} photo(s) still on their way");
            }
            if let Some(finish) = finish.take() {
                finish();
            }
            glib::ControlFlow::Break
        });
        glib::Propagation::Stop
    });
    // a kill (TERM, INT, HUP) closes the window as the user would, so the transfer streams
    // and the preview stop in their order (killed under a running preview, the streams leave
    // CAMSS unable to start it again until a reboot)
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let w = window.clone();
        // kept for the whole close: removed, a second TERM during it was a plain kill
        glib::unix_signal_add_local(sig, move || {
            if !CLOSING.load(Ordering::Relaxed) {
                w.close();
            }
            glib::ControlFlow::Continue
        });
    }

    let a = app.clone();
    let start = move || {
        // records left in /tmp by a camera that died mid-photo (its own are saved by now:
        // a closing camera keeps the lock until they are)
        if let Ok(d) = std::fs::read_dir("/tmp") {
            for e in d.flatten() {
                if e.file_name().to_string_lossy().starts_with("l16-shot-") {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        if let Some(c) = &a.ccb {
            let st = a.st.borrow();
            c.set(ccb::MODULE, 0);
            // the photos are dated in local time, as stock's (the driver's SET_TIME at each start)
            let offset = glib::DateTime::now_local().map_or(0, |d| (d.utc_offset().as_seconds()) as i32);
            c.set(ccb::UTC_OFFSET, offset);
            c.set(ccb::FLASH, st.flash as i32);
            c.set(ccb::METERING, st.metering as i32);
            c.set(ccb::ZOOM, 1000);
        }
        a.start_preview();
        a.apply_exposure();
        a.apply_wb();
        a.start_transfers_on_frame();
        a.refresh();
    };
    // opened again while the last camera was still stopping its streams (it gave up the app's
    // name at once, so this launch is a camera of its own): the driver once those are stopped
    if take_camera() {
        start();
    } else {
        eprintln!("nebula: waiting for the last camera to close");
        let mut start = Some(start);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            if !take_camera() {
                return glib::ControlFlow::Continue;
            }
            eprintln!("nebula: the last camera closed");
            if let Some(start) = start.take() {
                start();
            }
            glib::ControlFlow::Break
        });
    }
    if dev::demo() {
        window.set_default_size(960, 540);
    } else {
        window.fullscreen();
    }
    window.present();
    dev::hooks(&window);
    // a screen to show for the screenshots (L16_DEMO_VIEW, demo mode only)
    if let (true, Ok(view)) = (dev::demo(), std::env::var("L16_DEMO_VIEW")) {
        let a = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            for v in view.split(',') {
                match v {
                    "manual" => a.set_mode(Mode::Manual),
                    "iso-priority" => a.set_mode(Mode::Iso),
                    "histogram" => {
                        a.st.borrow_mut().histogram = true;
                        a.refresh();
                    }
                    "assist" => {
                        a.st.borrow_mut().assist = 3;
                        a.refresh();
                    }
                    "grid" => {
                        a.st.borrow_mut().grid = 1;
                        a.refresh();
                    }
                    "iso" | "shutter" | "ev" => {
                        a.st.borrow_mut().wheel = Some(match v {
                            "iso" => Dial::Iso,
                            "shutter" => Dial::Shutter,
                            _ => Dial::Ev,
                        });
                        a.place_flyout(match v {
                            "iso" => 0,
                            "shutter" => 1,
                            _ => 2,
                        });
                        a.update_wheels();
                        a.refresh();
                    }
                    "zoom" => a.set_zoom(70.0),
                    "portrait" => a.apply_quarter(1),
                    "picker" => a.open_picker(),
                    "contrast" => {
                        a.st.borrow_mut().contrast = 1;
                        a.refresh();
                    }
                    "thermal" => {
                        set_class(&a.thermal_turn, "off", false);
                        a.animate_thermal(1.0);
                    }
                    "lens" => {
                        a.blocked.store(0b00101, Ordering::Relaxed);
                    }
                    "stow" => a.set_stowed(true),
                    "system" => a.show_sidebar(true),
                    "settings1" | "settings3" | "settings4" => {
                        a.fill_settings();
                        a.settings_page.set_visible(true);
                        let k = match v { "settings1" => 1, "settings3" => 3, _ => 4 };
                        if let Some(row) = a.settings_nav.row_at_index(k) {
                            a.settings_nav.select_row(Some(&row));
                        }
                    }
                    "bubble" => {
                        let (t, _) = a.tool_note(Tool::Assist);
                        a.show_pill(a.tool_icon(Tool::Assist), &t, 600);
                    }
                    "hint" => a.show_alert("shake", icons::HAND_WAVE, "Hold steady", "The shutter is slow enough that a shaky hand will blur the photo. Brace the camera, or use a tripod.", 600),
                    "zoombar" => {
                        a.set_zoom(50.0);
                        a.st.borrow_mut().zoom_wheel_until = Some(Instant::now() + Duration::from_secs(60));
                        a.update_wheels();
                    }
                    "settings" => {
                        a.fill_settings();
                        a.settings_page.set_visible(true);
                    }
                    other => eprintln!("nebula: no demo view {other:?}"),
                }
            }
        });
    }
}

// While in front, a marker in the runtime directory: the touch strip is the camera's zoom
// then, and light-lfc-strip-volume leaves it alone (in the overview it is volume again)
fn front_marker(front: bool) {
    let path = glib::user_runtime_dir().join("l16-camera.front");
    if front {
        let _ = std::fs::write(&path, b"");
    } else {
        let _ = std::fs::remove_file(&path);
    }
}

// The camera, one app at a time: a lock held from the start until the process ends (its
// streams stopped). Taken once, the preview may start (start_preview).
static CAMERA_TAKEN: AtomicBool = AtomicBool::new(false);

fn take_camera() -> bool {
    if CAMERA_TAKEN.load(Ordering::Relaxed) {
        return true;
    }
    let path = glib::user_runtime_dir().join("l16-camera.lock");
    let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path) else {
        // no lock to be had: carry on as before
        CAMERA_TAKEN.store(true, Ordering::Relaxed);
        return true;
    };
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return false;
    }
    // held until the process ends
    std::mem::forget(f);
    CAMERA_TAKEN.store(true, Ordering::Relaxed);
    true
}

// the window closed and the streams stopping
static CLOSING: AtomicBool = AtomicBool::new(false);

fn main() -> glib::ExitCode {
    // started from the app grid, the output went to the console: to a file instead
    // (~/.cache/nebula.log; appended to, as a launch that only hands over to a running
    // camera is a process too; started afresh past 1 MB)
    if unsafe { libc::isatty(2) } == 1 {
        let path = glib::user_cache_dir().join("nebula.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::dup2(f.as_raw_fd(), 1);
                libc::dup2(f.as_raw_fd(), 2);
            }
        }
    }
    eprintln!("nebula: started (pid {})", std::process::id());
    // GTK redraws the whole window each frame: redrawing only what changed (the preview)
    // left the badges over it as flickering black bars
    if std::env::var_os("GSK_DEBUG").is_none() {
        std::env::set_var("GSK_DEBUG", "full-redraw");
    }
    gst::init().expect("gstreamer");
    let app = gtk::Application::builder().application_id("org.l16linux.Nebula").build();
    // launched again while running (the gallery's camera button): back to the window there is
    app.connect_activate(|app| {
        // logged: an activation while the app was closing (its streams stopping) left the
        // preview dead (2026-10-02)
        eprintln!("nebula: activated (window {})", app.active_window().is_some());
        if CLOSING.load(Ordering::Relaxed) {
            // closing, its name given up: a launch now is a camera of its own
            return;
        }
        match app.active_window() {
            Some(w) => w.present(),
            None => build(app),
        }
    });
    app.run()
}

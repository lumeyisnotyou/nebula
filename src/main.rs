// l16-camera: a camera app for the Light L16 on Linux, laid out after OpenLight (the L16's
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
mod geo;
mod gyro;
mod haptics;
mod icons;
mod input;
mod prox;
mod rotate;
mod settings;
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use canvas::Canvas;
use rotate::Rotator;
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
const BURSTS: &[u8] = &[1, 3, 6];
// zoom stops: OpenLight's primes, with the L16's real 70 mm B modules
const PRIMES: &[f64] = &[28.0, 35.0, 70.0, 150.0];
const ZOOM_MIN: f64 = 28.0;
const ZOOM_MAX: f64 = 150.0;
// the preview modules' focal lengths: A1, and B4 from 70 mm on (stock never previews on
// the 150 mm modules; it crops B4)
const MODULE_MM: [f64; 2] = [28.0, 70.0];
// the mode wheel's touch band: its labels (two either side of the chosen one)
const MODE_TOUCH_H: i32 = 320;
// amber: what the photographer has set; green: in focus, fine; red: clipping, warnings
const ACCENT: (f64, f64, f64) = (1.0, 0.690, 0.180); // #FFB02E
const STRIP_LEN: f64 = 768.0;
const HIST_BINS: usize = 64;

const CSS: &str = "
window.camera { background: #000; color: #f2f2ee; font-family: 'Adwaita Sans', 'Droid Sans', sans-serif; }
.mono, .hud-value, .set-value, .countdown, .burst-count { font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; }
.hud-cell { padding: 6px 0; }
.hud-unit { color: rgba(242,242,238,0.45); font-size: 10px; font-weight: 700; letter-spacing: 2px; }
.hud-value { color: #f2f2ee; font-size: 19px; font-weight: 700; }
.hud-value.fixed { color: #FFB02E; }
.hud-value.dim { color: rgba(242,242,238,0.35); }
.toolbar { background: rgba(14,14,16,0.82); border-radius: 18px; margin: 0 14px 12px 14px;
    border: 1px solid rgba(255,255,255,0.10); padding: 2px 6px; }
.toolbar button, button.flat-white { background: none; border: none; box-shadow: none; outline: none;
    color: #f2f2ee; font-size: 15px; font-weight: 600; min-width: 64px; min-height: 52px;
    padding: 0; border-radius: 14px; }
.toolbar button:active, button.flat-white:active { background: rgba(255,255,255,0.10); }
.toolbar button.on { color: #FFB02E; }
.options { background: rgba(14,14,16,0.88); margin-bottom: 8px; }
.mode-chip { font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 13px; font-weight: 700;
    letter-spacing: 2px; color: #FFB02E; border: 1px solid rgba(255,176,46,0.55); border-radius: 12px;
    padding: 3px 12px; }
.zoom-pill { background: rgba(14,14,16,0.72); border: 1px solid rgba(255,255,255,0.10);
    border-radius: 22px; padding: 3px; }
.zoom-chip { background: none; border: none; box-shadow: none; outline: none; padding: 0;
    color: rgba(242,242,238,0.75); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace;
    font-size: 13px; font-weight: 700; min-width: 46px; min-height: 36px; border-radius: 18px; }
.zoom-chip.active { background: rgba(255,176,46,0.18); color: #FFB02E; }
.status { color: #f2f2ee; font-size: 15px; font-weight: 600; background: rgba(14,14,16,0.82);
    border: 1px solid rgba(255,255,255,0.10); border-radius: 14px; padding: 5px 16px; }
.countdown { color: #f2f2ee; font-size: 110px; font-weight: 700; }
.thumb { border: 1px solid rgba(255,255,255,0.55); border-radius: 10px; }
.blackout { background: #000; }
.burst-screen { background: #000; }
.device-status label { color: rgba(242,242,238,0.75); font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace;
    font-size: 12px; font-weight: 600; }
.battery-screen { background: #000; }
.thermal-warning { color: #fff; font-size: 14px; font-weight: 700; background: rgba(200,40,40,0.85);
    border-radius: 14px; padding: 5px 16px; }
.battery-screen label { color: #f2f2ee; font-size: 22px; font-weight: 600; }
.burst-count { color: #f2f2ee; font-size: 56px; }
.burst-saving { color: rgba(242,242,238,0.8); font-size: 20px; }
.assist-badge { color: #FFB02E; font-size: 15px; }
.burst-badge { color: #FFB02E; font-family: 'Adwaita Mono', 'Droid Sans Mono', monospace; font-size: 12px;
    font-weight: 700; border: 1px solid rgba(255,176,46,0.6); border-radius: 8px; padding: 0 6px; }
.settings { background: #000; }
.settings list { background: #000; }
.settings row, .chooser row { padding: 16px 32px; border-bottom: 1px solid rgba(255,255,255,0.08);
    background: none; }
.settings row:active, .chooser row:active { background: rgba(255,255,255,0.08); }
.set-title { color: #f2f2ee; font-size: 17px; font-weight: 600; }
.set-sub { color: rgba(242,242,238,0.50); font-size: 13px; }
.set-value { color: #FFB02E; font-size: 15px; font-weight: 700; }
.set-chevron { color: rgba(242,242,238,0.45); font-size: 20px; }
.settings switch { background: rgba(255,255,255,0.18); border: none; }
.settings switch:checked { background: #FFB02E; }
.settings switch slider { background: #f2f2ee; border: none; box-shadow: none; }
.chooser { background: rgba(0,0,0,0.65); }
.chooser-card { background: #16161a; border-radius: 18px; border: 1px solid rgba(255,255,255,0.10); }
.chooser-card list { background: none; }
.chooser-title { color: rgba(242,242,238,0.5); font-size: 12px; font-weight: 700; letter-spacing: 2px;
    padding: 18px 32px 8px 32px; }
.chooser-check { color: #FFB02E; font-size: 18px; font-weight: 700; }
.spin { transition: transform 50ms ease-in; }
window.rot-cw .spin { transform: rotate(90deg); }
window.rot-ccw .spin { transform: rotate(-90deg); }
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

    // the mode wheel's label
    fn label(self) -> &'static str {
        ["auto", "iso priority", "shutter priority", "manual"][self.index()]
    }

    // the toolbar opener's (and the settings file's)
    fn short(self) -> &'static str {
        ["auto", "iso", "shutter", "manual"][self.index()]
    }

    // the dials above and below the shutter (stock's getTopControlWheel / getBottomControlWheel)
    fn dials(self) -> (Option<Dial>, Dial) {
        match self {
            Mode::Auto => (None, Dial::Ev),
            Mode::Iso => (Some(Dial::Iso), Dial::Ev),
            Mode::Shutter => (Some(Dial::Ev), Dial::Shutter),
            Mode::Manual => (Some(Dial::Iso), Dial::Shutter),
        }
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
    mode_pos: f64, // the mode wheel's position, 0 (auto) to 1 (manual)
    mode_start: f64,
    mode_swiped: bool,
    iso: f64,     // position, see iso_at
    shutter: f64, // position, see secs_at
    ev: f64,      // position, see ev_at
    zoom: f64,
    module: usize,
    timer: usize,
    grid: u8, // 0 off, 1 3x3, 2 golden ratio
    histogram: bool,
    assist: u8, // focus peaking (1) and zebras (2), as bits
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
             wb={}\nmetering={}\ncaf={}\nstacked={}\nexposure_info={}\ninverse_wheel={}\nhaptics={}\ncontinuous={}\nstrip_zoom={}\ntoolbar={}\ntool_cycle={}\nlens_warn={}\ndevice_status={}\npocket={}\ngeotag={}\n",
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
    }
}

// a row of the settings screen: a switch, or a value that opens a list to choose from
enum SettingKind {
    Switch(fn(&State) -> bool, fn(&mut State, bool)),
    Choice(&'static [&'static str], fn(&State) -> usize, fn(&mut State, usize)),
}

struct SettingRow {
    title: &'static str,
    sub: &'static str,
    kind: SettingKind,
}

const SETTINGS: &[SettingRow] = &[
    SettingRow {
        title: "Metering",
        sub: "Where auto exposure meters: the centre, the spot you tap, or the whole frame",
        kind: SettingKind::Choice(
            &["Centre-weighted", "Touch", "Whole frame"],
            |s| s.metering as usize,
            |s, v| s.metering = v as u8,
        ),
    },
    SettingRow {
        title: "Continuous focus",
        sub: "Refocus when the scene changes (AF-D), outside manual mode",
        kind: SettingKind::Switch(|s| s.caf, |s, v| s.caf = v),
    },
    SettingRow {
        title: "Stacked capture",
        sub: "In low light, several exposures per module for less noise",
        kind: SettingKind::Switch(|s| s.stacked, |s, v| s.stacked = v),
    },
    SettingRow {
        title: "Exposure info",
        sub: "EV, ISO, shutter and focal length beside the preview",
        kind: SettingKind::Switch(|s| s.exposure_info, |s, v| s.exposure_info = v),
    },
    SettingRow {
        title: "Exposure steps",
        sub: "ISO and shutter in stock's 1/3 stops, or anywhere in between",
        kind: SettingKind::Choice(
            &["1/3 stop", "Continuous"],
            |s| s.continuous as usize,
            |s, v| s.continuous = v == 1,
        ),
    },
    SettingRow {
        title: "Haptics",
        sub: "Vibration as the dials and the zoom turn",
        kind: SettingKind::Choice(
            &["Off", "Normal", "Strong"],
            |s| s.haptics as usize,
            |s, v| s.haptics = v as u8,
        ),
    },
    SettingRow {
        title: "Inverse wheel scroll",
        sub: "Turn the exposure wheels the other way",
        kind: SettingKind::Switch(|s| s.inverse_wheel, |s, v| s.inverse_wheel = v),
    },
    SettingRow {
        title: "Device status",
        sub: "Battery and captures left in the corner of the viewfinder",
        kind: SettingKind::Switch(|s| s.device_status, |s, v| s.device_status = v),
    },
    SettingRow {
        title: "Geotagging",
        sub: "Record where photos are taken (the camera's GPS, through location services)",
        kind: SettingKind::Switch(|s| s.geotag, |s, v| s.geotag = v),
    },
    SettingRow {
        title: "Pocket detection",
        sub: "Close the camera after 30 s in a pocket (the lenses covered, in the dark)",
        kind: SettingKind::Switch(|s| s.pocket, |s, v| s.pocket = v),
    },
    SettingRow {
        title: "Lens blocked warning",
        sub: "Warn when a finger covers the camera modules (the sensors around them)",
        kind: SettingKind::Choice(
            &["Off", "On", "On, with buzz"],
            |s| s.lens_warning as usize,
            |s, v| s.lens_warning = v as u8,
        ),
    },
    SettingRow {
        title: "Touch strip",
        sub: "Zoom with the touch strip",
        kind: SettingKind::Switch(|s| s.strip_zoom, |s, v| s.strip_zoom = v),
    },
];

// what the toolbar can hold (the toolbar editor chooses which, and their order)
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
}

const TOOLS: [Tool; 8] =
    [Tool::Flash, Tool::Wb, Tool::Timer, Tool::Grid, Tool::Histogram, Tool::Assist, Tool::Burst, Tool::Afd];

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
        }
    }

    fn title(self) -> &'static str {
        match self {
            Tool::Flash => "Flash",
            Tool::Wb => "White balance",
            Tool::Timer => "Timer",
            Tool::Grid => "Grid",
            Tool::Histogram => "Histogram",
            Tool::Burst => "Burst",
            Tool::Assist => "Focus peaking and zebras",
            Tool::Afd => "Continuous focus (AF-D)",
        }
    }

    fn icon(self) -> char {
        match self {
            Tool::Flash => icons::FLASH,
            Tool::Wb => icons::WB[0],
            Tool::Timer => icons::TIMER,
            Tool::Grid => icons::GRID,
            Tool::Histogram => icons::HISTOGRAM,
            Tool::Burst => icons::BURST,
            Tool::Assist => icons::ASSIST,
            Tool::Afd => icons::FOCUS_AUTO,
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
            Tool::Histogram | Tool::Afd => None,
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
    lens_badge: gtk::DrawingArea,
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
    status_turn: Rotator,
    thermal_turn: Rotator,
    // the lens pill (the primes as buttons, the zoom on the nearest) at the preview's bottom edge
    zoom_chips: Vec<gtk::Button>,
    zoom_turn: Rotator,
    // in front: the screen kept on (an idle inhibitor's cookie) and the display held in
    // landscape (the rotation lock and transform it had, given back after)
    idle_cookie: Cell<u32>,
    landscape_held: RefCell<Option<(bool, Option<String>)>>,
    rotators: Vec<Rotator>,
    geo: RefCell<geo::Geo>,
    storage_label: gtk::Label,
    battery_label: gtk::Label,
    battery_screen: gtk::Box,
    thermal_warning: gtk::Label,
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
    wheels: Canvas,
    hud: Vec<gtk::Label>,
    top: Canvas,
    bottom: Canvas,
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
    // what the dials and the shutter button were last drawn for (refresh)
    dials_shown: Cell<Option<(Mode, Option<Dial>, bool)>>,
    moon_badge: gtk::Label,
    shake_badge: gtk::Label,
    mode_label: gtk::Label,
    toolbar: gtk::Revealer,
    // a multi-option setting's choices, in a row above the toolbar
    options: gtk::Revealer,
    options_row: gtk::Box,
    options_for: Cell<Option<Opt>>,
    preview_gain: Cell<f32>,
    right: gtk::Box,
    mode_wheel: gtk::DrawingArea,
    mode_touch: gtk::Box,
    timer_btn: gtk::Button,
    grid_btn: gtk::Button,
    hist_btn: gtk::Button,
    hist: RefCell<Vec<u32>>,
    burst_btn: gtk::Button,
    flash_btn: gtk::Button,
    wb_btn: gtk::Button,
    afd_btn: gtk::Button,
    assist_btn: gtk::Button,
    tools_box: gtk::Box,
    // the settings screen, the list a value is chosen from over it, and the toolbar editor
    settings_list: gtk::ListBox,
    setting_taps: RefCell<Vec<Rc<dyn Fn()>>>,
    chooser: gtk::Box,
    chooser_title: gtk::Label,
    chooser_list: gtk::ListBox,
    chooser_pick: RefCell<Option<Rc<dyn Fn(usize)>>>,
    editor: gtk::Box,
    editor_list: gtk::ListBox,
    cal: wb::Calibration,
    motor: haptics::Haptics,
    // a photo's view preferences for the LRI (white balance, exposure), by its directory
    photo_args: RefCell<HashMap<PathBuf, Vec<String>>>,
    hud_box: gtk::Box,
    settings_page: gtk::Overlay,
    last_saved: RefCell<String>,
    // logind's sleep inhibitor while photos are on their way (dropped: released)
    sleep_inhibitor: RefCell<Option<std::os::fd::OwnedFd>>,
    status: gtk::Label,
    countdown: gtk::Label,
}

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
    font.set_absolute_size(size * gtk::pango::SCALE as f64);
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

// text() turned @q quarters clockwise for portrait, on the same side of (x, y)
fn text_q(cr: &cairo::Context, s: &str, x: f64, y: f64, size: f64, align: f64, q: i32) {
    if q == 0 {
        return text(cr, s, x, y, size, align);
    }
    cr.save().ok();
    cr.translate(x - (align - 0.5) * size * 1.2, y);
    cr.rotate(q as f64 * PI / 2.0);
    text(cr, s, 0.0, 0.0, size, 0.5);
    cr.restore().ok();
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
        let ev = if st.mode == Mode::Manual { "–".to_string() } else { fmt_ev(ev_at(st.ev)) };
        let iso = if st.mode.fixes_iso() { iso_at(st.iso) } else { st.live_iso };
        let secs = if st.mode.fixes_shutter() { secs_at(st.shutter) } else { st.live_secs };
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
        self.hud[0].set_text(&ev);
        for (l, fixed, dim) in [
            (&self.hud[0], st.mode != Mode::Manual && st.ev != 0.5 && ev_at(st.ev) != 0, st.mode == Mode::Manual),
            (&self.hud[1], st.mode.fixes_iso(), iso <= 0),
            (&self.hud[2], st.mode.fixes_shutter(), secs <= 0.0),
        ] {
            if fixed { l.add_css_class("fixed") } else { l.remove_css_class("fixed") }
            if dim { l.add_css_class("dim") } else { l.remove_css_class("dim") }
        }
        self.hud[1].set_text(&if iso > 0 { iso.to_string() } else { "–".into() });
        self.hud[2].set_text(&if secs > 0.0 { fmt_secs(secs) } else { "–".into() });
        self.hud[3].set_text(&format!("{:.0}", st.zoom));
        self.focal.store((st.zoom * 10.0) as u32, Ordering::Relaxed);
        self.mode_label.set_text(&st.mode.short().to_uppercase());
        let t = TIMERS[st.timer];
        icons::set(&self.timer_btn, if t == 0 { icons::TIMER_OFF } else { icons::TIMER }, &if t == 0 { String::new() } else { format!("{t}s") });
        if t == 0 {
            self.timer_btn.remove_css_class("on");
        } else {
            self.timer_btn.add_css_class("on");
        }
        icons::set(&self.grid_btn, if st.grid == 0 { icons::GRID_OFF } else { icons::GRID }, ["", "", "φ"][st.grid as usize]);
        icons::set(&self.hist_btn, icons::HISTOGRAM, "");
        if st.histogram {
            self.hist_btn.add_css_class("on");
        } else {
            self.hist_btn.remove_css_class("on");
        }
        icons::set(&self.flash_btn, [icons::FLASH_OFF, icons::FLASH_AUTO, icons::FLASH][st.flash as usize], "");
        icons::set(&self.wb_btn, icons::WB[st.wb], "");
        if st.wb > 0 {
            self.wb_btn.add_css_class("on");
        } else {
            self.wb_btn.remove_css_class("on");
        }
        self.hud_box.set_opacity(if st.exposure_info { 1.0 } else { 0.0 });
        if st.flash > 0 {
            self.flash_btn.add_css_class("on");
        } else {
            self.flash_btn.remove_css_class("on");
        }
        let b = BURSTS[st.burst];
        icons::set(&self.burst_btn, icons::BURST, &if b > 1 { b.to_string() } else { String::new() });
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
        icons::set(&self.assist_btn, if st.assist == 0 { icons::ASSIST_OFF } else { icons::ASSIST }, ["", "PK", "ZB", "PK+ZB"][st.assist as usize]);
        if st.assist > 0 {
            self.assist_btn.add_css_class("on");
        } else {
            self.assist_btn.remove_css_class("on");
        }
        self.view.set_assist(st.assist);
        if st.caf {
            self.afd_btn.add_css_class("on");
        } else {
            self.afd_btn.remove_css_class("on");
        }
        let saved = st.saved();
        let dials = (st.mode, st.wheel, st.busy);
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
        if self.dials_shown.replace(Some(dials)) != Some(dials) {
            self.top.queue_draw();
            self.bottom.queue_draw();
            self.shutter.queue_draw();
        }
        // the grid and the histogram: only when switched (the histogram's updates redraw it)
        if self.marks_grid.replace(grid) != grid {
            self.marks.set_visible(grid & 0xf != 0);
            self.marks.queue_draw();
            self.hist_area.set_visible(grid & 0x10 != 0);
            self.hist_area.queue_draw();
        }
    }

    fn show_status(self: &Rc<Self>, msg: &str, secs: u64) {
        self.status.set_text(msg);
        self.status.set_visible(true);
        if secs > 0 {
            let app = self.clone();
            let msg = msg.to_string();
            glib::timeout_add_local_once(Duration::from_secs(secs), move || {
                if app.status.text() == msg.as_str() {
                    app.status.set_visible(false);
                }
            });
        }
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
        eprintln!("l16-camera2: white balance {} (module {module}): {gains:?}", wb::PRESETS[preset]);
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
        }
    }

    // the toolbar: the chosen buttons, in their order
    fn layout_toolbar(&self) {
        while let Some(c) = self.tools_box.first_child() {
            self.tools_box.remove(&c);
        }
        let tools = self.st.borrow().tools.clone();
        for t in tools {
            self.tools_box.append(self.tool_button(t));
        }
    }

    fn tool_tap(self: &Rc<Self>, t: Tool) {
        match t.opt() {
            Some(o) if self.st.borrow().tool_cycle => {
                let (choices, now) = self.choices(o);
                self.choose(o, (now + 1) % choices.len());
            }
            Some(o) => self.show_options(Some(o)),
            None => {
                self.show_options(None);
                {
                    let mut st = self.st.borrow_mut();
                    match t {
                        Tool::Histogram => st.histogram = !st.histogram,
                        _ => st.caf = !st.caf,
                    }
                }
                self.refresh();
            }
        }
    }

    // after a setting changes: the driver's side of it, the screen, the settings file
    fn setting_changed(&self) {
        let meter = self.st.borrow().metering;
        let _ = self.ctl_tx.send((ccb::METERING, meter as i32));
        self.refresh();
    }

    // a settings row: title, explanation and what goes on the right; its tap
    fn setting_row(&self, title: &str, sub: &str, right: &[gtk::Widget], tap: Rc<dyn Fn()>) {
        let t = gtk::Label::new(Some(title));
        t.add_css_class("set-title");
        t.set_halign(gtk::Align::Start);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.append(&t);
        if !sub.is_empty() {
            let d = gtk::Label::new(Some(sub));
            d.add_css_class("set-sub");
            d.set_halign(gtk::Align::Start);
            text.append(&d);
        }
        text.set_hexpand(true);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        line.append(&text);
        for w in right {
            line.append(w);
        }
        self.settings_list.append(&line);
        self.setting_taps.borrow_mut().push(tap);
    }

    fn switch_row(self: &Rc<Self>, title: &str, sub: &str, get: fn(&State) -> bool, set: fn(&mut State, bool)) {
        let sw = gtk::Switch::new();
        sw.set_active(get(&self.st.borrow()));
        sw.set_valign(gtk::Align::Center);
        sw.set_can_target(false); // the row's tap flips it
        let a = self.clone();
        sw.connect_active_notify(move |sw| {
            set(&mut a.st.borrow_mut(), sw.is_active());
            a.setting_changed();
        });
        let s = sw.clone();
        self.setting_row(title, sub, &[sw.upcast()], Rc::new(move || s.set_active(!s.is_active())));
    }

    fn choice_row(self: &Rc<Self>, title: &'static str, sub: &str, options: Vec<String>, now: usize, pick: Rc<dyn Fn(usize)>) {
        let value = gtk::Label::new(options.get(now).map(String::as_str));
        value.add_css_class("set-value");
        let chevron = icons::label(icons::CHEVRON_RIGHT);
        chevron.add_css_class("set-chevron");
        let a = self.clone();
        self.setting_row(title, sub, &[value.upcast(), chevron.upcast()], Rc::new(move || {
            a.open_chooser(title, &options, now, pick.clone());
        }));
    }

    fn link_row(&self, title: &str, sub: &str, tap: Rc<dyn Fn()>) {
        let chevron = icons::label(icons::CHEVRON_RIGHT);
        chevron.add_css_class("set-chevron");
        self.setting_row(title, sub, &[chevron.upcast()], tap);
    }

    fn open_chooser(&self, title: &str, options: &[String], now: usize, pick: Rc<dyn Fn(usize)>) {
        self.chooser_title.set_text(title);
        while let Some(c) = self.chooser_list.first_child() {
            self.chooser_list.remove(&c);
        }
        for (k, o) in options.iter().enumerate() {
            let l = gtk::Label::new(Some(o));
            l.add_css_class("set-title");
            l.set_halign(gtk::Align::Start);
            l.set_hexpand(true);
            let check = if k == now { icons::label(icons::CHECK) } else { gtk::Label::new(None) };
            check.add_css_class("chooser-check");
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            b.append(&l);
            b.append(&check);
            self.chooser_list.append(&b);
        }
        *self.chooser_pick.borrow_mut() = Some(pick);
        self.chooser.set_visible(true);
    }

    // the settings screen: first the toolbar's settings that aren't on the toolbar, then
    // the toolbar's own, then the rest
    fn fill_settings(self: &Rc<Self>) {
        while let Some(c) = self.settings_list.first_child() {
            self.settings_list.remove(&c);
        }
        self.setting_taps.borrow_mut().clear();
        let hidden: Vec<Tool> = TOOLS
            .into_iter()
            // (AF-D has its row among the rest)
            .filter(|t| *t != Tool::Afd && !self.st.borrow().tools.contains(t))
            .collect();
        for t in hidden {
            match t.opt() {
                Some(o) => {
                    let (choices, now) = self.choices(o);
                    let names = choices.into_iter().map(|(_, n)| n).collect();
                    let a = self.clone();
                    self.choice_row(t.title(), "", names, now, Rc::new(move |k| a.choose(o, k)));
                }
                None if t == Tool::Histogram => {
                    self.switch_row(t.title(), "", |s| s.histogram, |s, v| s.histogram = v)
                }
                None => self.switch_row(t.title(), "", |s| s.caf, |s, v| s.caf = v),
            }
        }
        let a = self.clone();
        self.link_row(
            "Toolbar",
            "Which buttons the toolbar has, and their order",
            Rc::new(move || {
                a.fill_editor();
                a.editor.set_visible(true);
            }),
        );
        let a = self.clone();
        let cycle = self.st.borrow().tool_cycle as usize;
        self.choice_row(
            "Toolbar buttons",
            "A button with several settings shows them in a row above the toolbar, or steps to the next",
            vec!["Show choices".into(), "Cycle".into()],
            cycle,
            Rc::new(move |k| a.st.borrow_mut().tool_cycle = k == 1),
        );
        for row in SETTINGS {
            match row.kind {
                SettingKind::Switch(get, set) => self.switch_row(row.title, row.sub, get, set),
                SettingKind::Choice(options, get, set) => {
                    let now = get(&self.st.borrow());
                    let a = self.clone();
                    let names = options.iter().map(|o| o.to_string()).collect();
                    self.choice_row(row.title, row.sub, names, now, Rc::new(move |k| set(&mut a.st.borrow_mut(), k)));
                }
            }
        }
    }

    // the toolbar editor: the toolbar's buttons in order (up and down move them), then the
    // others; the switch puts one on the toolbar or takes it off
    fn fill_editor(self: &Rc<Self>) {
        while let Some(c) = self.editor_list.first_child() {
            self.editor_list.remove(&c);
        }
        let shown = self.st.borrow().tools.clone();
        let rest = TOOLS.into_iter().filter(|t| !shown.contains(t));
        for t in shown.iter().copied().chain(rest) {
            let on = shown.contains(&t);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            let icon = icons::label(t.icon());
            icon.add_css_class("set-title");
            let title = gtk::Label::new(Some(t.title()));
            title.add_css_class("set-title");
            title.set_halign(gtk::Align::Start);
            title.set_hexpand(true);
            line.append(&icon);
            line.append(&title);
            for (glyph, by) in [(icons::CHEVRON_UP, -1i32), (icons::CHEVRON_DOWN, 1)] {
                let b = icons::button(glyph, "");
                b.add_css_class("flat-white");
                let i = shown.iter().position(|s| *s == t);
                let can = i.is_some_and(|i| (0..shown.len() as i32).contains(&(i as i32 + by)));
                b.set_opacity(if can { 1.0 } else { 0.0 });
                b.set_sensitive(can);
                let a = self.clone();
                b.connect_clicked(move |_| {
                    if let Some(i) = i {
                        a.st.borrow_mut().tools.swap(i, (i as i32 + by) as usize);
                        a.editor_changed();
                    }
                });
                line.append(&b);
            }
            let sw = gtk::Switch::new();
            sw.set_active(on);
            sw.set_valign(gtk::Align::Center);
            let a = self.clone();
            sw.connect_active_notify(move |sw| {
                {
                    let mut st = a.st.borrow_mut();
                    st.tools.retain(|s| *s != t);
                    if sw.is_active() {
                        st.tools.push(t);
                    }
                }
                a.editor_changed();
            });
            line.append(&sw);
            self.editor_list.append(&line);
        }
    }

    fn editor_changed(self: &Rc<Self>) {
        self.layout_toolbar();
        self.refresh();
        let a = self.clone();
        glib::idle_add_local_once(move || a.fill_editor());
    }

    // a toolbar button with choices: their row, or (tapped again) none
    fn show_options(self: &Rc<Self>, o: Option<Opt>) {
        let o = if o.is_some() && self.options_for.get() == o { None } else { o };
        self.options_for.set(o);
        self.options.set_reveal_child(o.is_some());
        let Some(o) = o else { return };
        while let Some(c) = self.options_row.first_child() {
            self.options_row.remove(&c);
        }
        let (choices, now) = self.choices(o);
        for (k, (icon, name)) in choices.into_iter().enumerate() {
            let b = icons::button(icon, &name);
            b.add_css_class("spin");
            if k == now {
                b.add_css_class("on");
            }
            let a = self.clone();
            b.connect_clicked(move |_| {
                a.choose(o, k);
                a.show_options(None);
            });
            self.options_row.append(&b);
        }
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
        }
        self.refresh();
    }

    // stock's toolbar: opening it swaps the right-hand column (last photo, dials, shutter)
    // for the mode wheel (only while no photo is being taken)
    fn show_toolbar(&self, open: bool) {
        if open && self.st.borrow().busy {
            return;
        }
        self.toolbar.set_reveal_child(open);
        self.zoom_turn.set_visible(!open);
        if !open {
            self.options_for.set(None);
            self.options.set_reveal_child(false);
        }
        self.right.set_opacity(if open { 0.0 } else { 1.0 });
        self.right.set_can_target(!open);
        self.mode_wheel.set_visible(open);
        self.mode_touch.set_visible(open);
        let pos = self.st.borrow().mode.index() as f64 / (MODES.len() - 1) as f64;
        self.st.borrow_mut().mode_pos = pos;
        self.mode_wheel.queue_draw();
    }

    // the mode wheel's positions: 0 for auto to 1 for manual, a mode per 1/3
    // @apply: send the mode to the camera as the wheel passes it, as stock does (the driver
    // sends only what changed: one or two messages a mode); false only re-snaps the wheel
    fn set_mode_pos(&self, pos: f64, apply: bool) {
        let pos = pos.clamp(0.0, 1.0);
        let max = (MODES.len() - 1) as f64;
        let mode = MODES[(pos * max).round() as usize];
        self.st.borrow_mut().mode_pos = pos;
        // the mode changes as the wheel passes half way to it, as stock's
        if apply {
            self.set_mode(mode);
        } else if mode != self.st.borrow().mode {
            self.st.borrow_mut().mode = mode;
            self.refresh();
        }
        self.mode_wheel.queue_draw();
    }

    // stock's ModeWheel (landscape) in this screen's units (its pixels / 1.75): the modes as
    // text on a drum down the right edge, the chosen one level with a white bar at the edge
    fn mode_item_y(pos: f64, idx: usize, h: f64, step: f64) -> f64 {
        let max = (MODES.len() - 1) as f64;
        let th = step.to_radians() * (idx as f64 - max * pos);
        h / 2.0 - th.sin() * h * th.cos().powi(5)
    }

    // portrait or landscape, from iio-sensor-proxy: the window's class turns the icons in
    // place ("spin"), the rotators re-lay their text out, the wheels turn their labels
    fn follow_orientation(&self, accel: &gtk::gio::DBusProxy) {
        let o = accel.cached_property("AccelerometerOrientation").and_then(|v| v.get::<String>());
        let Some(q) = o.as_deref().and_then(quarter_for) else { return };
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
        self.lens_badge.queue_draw();
        self.wheels.queue_draw();
        self.mode_wheel.queue_draw();
    }

    // the status line along the preview's top edge as the camera is held: the top, or the
    // left with the shutter down (turned -90), the right with it up; the overheating
    // warning along the opposite edge, so neither covers the middle of the frame
    fn place_status(&self, q: i32) {
        place_on_edge(&self.status_turn, q, true, 14);
        place_on_edge(&self.thermal_turn, q, false, 70);
        place_on_edge(&self.zoom_turn, q, false, 14);
    }

    // degrees between the mode wheel's labels (stock's 2 and 4 dip, as angles)
    fn mode_step(&self) -> f64 {
        if self.quarter.get() == 0 { 6.0 } else { 12.0 }
    }

    fn draw_mode_wheel(&self, cr: &cairo::Context, w: f64, h: f64) {
        let pos = self.st.borrow().mode_pos;
        let (q, step) = (self.quarter.get(), self.mode_step());
        let label_of = |m: Mode| if q == 0 { m.label() } else { m.short() };
        let upper = |m: Mode| label_of(m).to_uppercase();
        // the strip's shade: clear at its left, a quarter black at the edge
        let g = cairo::LinearGradient::new(0.0, 0.0, w, 0.0);
        g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
        g.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.25);
        let _ = cr.set_source(&g);
        cr.rectangle(0.0, 0.0, w, h);
        let _ = cr.fill();
        cr.set_source_rgb(ACCENT.0, ACCENT.1, ACCENT.2);
        rounded(cr, w - 6.0, h / 2.0 - 24.0, 12.0, 48.0, 3.0);
        let _ = cr.fill();
        let max = (MODES.len() - 1) as f64;
        let base = (max * pos).floor() as i64;
        cr.select_font_face("Adwaita Mono", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        // the labels' size, smaller if the longest wouldn't fit
        cr.set_font_size(36.0);
        let widest = MODES
            .iter()
            .filter_map(|m| cr.text_extents(&upper(*m)).ok())
            .map(|e| e.width())
            .fold(0.0, f64::max);
        // turned, a label's length runs along the wheel: within the gap to the next one
        let room = if q == 0 { w - 48.0 - 16.0 } else { 0.85 * step.to_radians().sin() * h };
        let size = 36.0 * (room / widest).min(1.0);
        for i in -2i64..=2 {
            let idx = base + i;
            if idx < 0 || idx > max as i64 {
                continue;
            }
            let th = step.to_radians() * (idx as f64 - max * pos);
            let y = Self::mode_item_y(pos, idx as usize, h, step);
            // stock's perspective: the slot's exponent, and neighbours at 3/4 alpha
            let k = if i == 0 { 26 } else { 52 / i.abs() as i32 };
            let alpha = th.cos() * if i == 0 { 1.0 } else { 0.75 };
            let label = upper(MODES[idx as usize]);
            let label = label.as_str();
            cr.set_font_size(size * th.cos().powi(k));
            let Ok(e) = cr.text_extents(label) else { continue };
            if q == 0 {
                cr.move_to(w - e.width() - 48.0 - e.x_bearing(), y - e.height() / 2.0 - e.y_bearing());
                cr.text_path(label);
            } else {
                // turned about its centre, a text height in from the edge mark
                cr.save().ok();
                cr.translate(w - 48.0 - e.height() / 2.0, y);
                cr.rotate(q as f64 * PI / 2.0);
                cr.move_to(-e.width() / 2.0 - e.x_bearing(), -e.height() / 2.0 - e.y_bearing());
                cr.text_path(label);
                cr.restore().ok();
            }
            cr.set_source_rgba(0.0, 0.0, 0.0, alpha * 0.5);
            cr.set_line_width(3.0);
            let _ = cr.stroke_preserve();
            if i == 0 {
                cr.set_source_rgba(ACCENT.0, ACCENT.1, ACCENT.2, alpha);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
            }
            let _ = cr.fill();
        }
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

    fn set_zoom(self: &Rc<Self>, zoom: f64) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
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
        self.settings_page.set_visible(false);
        self.show_toolbar(false);
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
        let room = free > (frames as u64 + 1) * 300 << 20;
        if !room {
            self.show_status("waiting for photos to save", 2);
        }
        room
    }

    fn capture(self: &Rc<Self>) {
        self.hold_sleep();
        self.feedback("camera-shutter");
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
                "l16-camera2: captured {} in {:.2} s: records {:?} (burst {burst}, status {})",
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
                eprintln!("l16-camera2: transferred {} in {:.2} s", dir.display(), t.elapsed().as_secs_f64());
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
                Err(e) => eprintln!("l16-camera2: sleep inhibitor: {e}"),
            },
            Ok((_, None)) => eprintln!("l16-camera2: sleep inhibitor: no fd"),
            Err(e) => eprintln!("l16-camera2: sleep inhibitor: {e}"),
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
            Some(&("org.l16linux.Camera2", event, hints, -1i32).to_variant()),
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
            eprintln!("l16-camera2: camera modules at {} C: cooling off", temp.unwrap_or(0));
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
        self.thermal_warning.set_visible(level == 1);
        self.hot_screen.set_visible(level == 2);
        self.follow_screen();
    }

    // a lens covered: stock's warning and its buzz (every pass of the fast loop)
    fn lens_check(&self) {
        let mask = self.blocked.load(Ordering::Relaxed);
        let (shown, warn) = (self.st.borrow().lens_mask, self.st.borrow().lens_warning);
        let mask = if warn > 0 { mask } else { 0 };
        if mask != shown {
            self.st.borrow_mut().lens_mask = mask;
            self.lens_badge.set_visible(mask != 0);
            self.lens_badge.queue_draw();
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
            self.tripod_badge.set_visible(still);
        }
        // stock's in-pocket check (BasePreviewFragment): two or more lenses covered and under
        // 2 lux for 30 s: say so and close
        let lux = self
            .light
            .as_ref()
            .and_then(|l| l.cached_property("LightLevel"))
            .and_then(|v| v.get::<f64>())
            .unwrap_or(f64::MAX);
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
                "l16-camera2: in a pocket (lenses covered {:#04b}, {lux:.1} lux, 30 s): blanking the screen",
                self.blocked.load(Ordering::Relaxed)
            );
            self.status.set_visible(false);
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
        self.shake_badge.set_visible(shake);
        // the moon: a stacked capture ahead (only where stacking is on: auto, the setting)
        let stacking = self.st.borrow().stacked && self.st.borrow().mode == Mode::Auto;
        self.moon_badge.set_visible(stacking && self.metered[3].load(Ordering::Relaxed) == 1);
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
            eprintln!("l16-camera2: first preview frame");
            if !a.st.borrow().asleep && a.transfers.borrow().is_none() {
                a.start_transfers();
            }
        });
        *self.transfers_wait.borrow_mut() = Some(id);
    }

    // the photo transfer streams, beside the preview (done: Stage::Transferred); started after it
    fn start_transfers(&self) {
        let (done_tx, done_rx) = mpsc::channel();
        match transfer::Transfers::start(done_tx) {
            Ok(t) => *self.transfers.borrow_mut() = Some(t),
            Err(e) => {
                self.status.set_text(&format!("no photo transfers: {e}"));
                self.status.set_visible(true);
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
            eprintln!("l16-camera2: sleep (screen {screen}, away {away}): stopping");
            self.st.borrow_mut().asleep = true;
            self.gyro_on.store(false, Ordering::Relaxed);
            if let Some(mut t) = self.transfers.borrow_mut().take() {
                t.stop();
            }
            self.stop_preview();
            eprintln!("l16-camera2: sleep: preview stopped");
        } else if on && asleep {
            eprintln!("l16-camera2: wake (screen {screen}, front {front}, seen {seen}): starting the preview");
            self.st.borrow_mut().asleep = false;
            self.gyro_on.store(true, Ordering::Relaxed);
            let r = self.pipeline.set_state(gst::State::Playing);
            eprintln!("l16-camera2: wake: set_state(Playing) = {r:?}");
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
            eprintln!("l16-camera2: display held in landscape (was lock {was}, transform {transform:?})");
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
        eprintln!("l16-camera2: display given back (lock {was})");
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
                let (down, last) = {
                    let st = self.st.borrow();
                    (st.strip_down, st.strip_x)
                };
                if !down {
                    let mut st = self.st.borrow_mut();
                    st.strip_down = true;
                    st.strip_t0 = Instant::now();
                    st.strip_x0 = x;
                    st.strip_x = x;
                } else {
                    self.st.borrow_mut().strip_x = x;
                    let z = self.st.borrow().zoom;
                    // OpenLight: a full strip length zooms 2.3x
                    self.set_zoom(z * 2.3f64.powf((x - last) as f64 / STRIP_LEN));
                }
            }
            input::Ev::StripTouch(true) => {}
            input::Ev::StripTouch(false) => {
                let (tap, x0) = {
                    let mut st = self.st.borrow_mut();
                    st.strip_down = false;
                    let tap = st.strip_t0.elapsed() < Duration::from_millis(300)
                        && (st.strip_x - st.strip_x0).abs() < 30;
                    (tap, st.strip_x0)
                };
                // taps on the ends step between the primes
                if tap && x0 < 100 {
                    self.step_prime(false);
                } else if tap && x0 > 700 {
                    self.step_prime(true);
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

    // stock's lens-blocked warning (proximity_sensor_notification_layout): the camera's back
    // as seen through the screen (its cut corner top right), a ringed dot at each covered
    // sensor: ch0-2 down the left edge, ch3 top centre, ch4 bottom centre; "lens blocked"
    // below it. The camera's back never turns (its dots are where the sensors are); in
    // portrait only the words turn, and go below it as the camera is held
    fn draw_lens_blocked(&self, cr: &cairo::Context, w: f64, h: f64) {
        let mask = self.st.borrow().lens_mask;
        let q = self.quarter.get();
        let (bw, bh) = (160.0, 93.0);
        // portrait: a column beside the body for the turned words (clear of the dots on
        // the left edge, which stand 16 out)
        let side = 40.0;
        let (x0, y0) = match q {
            0 => ((w - bw) / 2.0, 12.0),
            -1 => ((w - bw - side) / 2.0, (h - bh) / 2.0),
            _ => ((w - bw - side) / 2.0 + side, (h - bh) / 2.0),
        };
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        match q {
            0 => rounded(cr, x0 - 12.0, 0.0, bw + 24.0, bh + 52.0, 12.0),
            -1 => rounded(cr, x0 - 12.0, y0 - 12.0, bw + 24.0 + side, bh + 24.0, 12.0),
            _ => rounded(cr, x0 - 12.0 - side, y0 - 12.0, bw + 24.0 + side, bh + 24.0, 12.0),
        }
        let _ = cr.fill();
        // the body: rounded corners, the top right one cut
        let (r, cut) = (6.0, 22.0);
        cr.new_path();
        cr.arc(x0 + r, y0 + r, r, PI, 1.5 * PI);
        cr.line_to(x0 + bw - cut, y0);
        cr.line_to(x0 + bw, y0 + cut * 0.55);
        cr.arc(x0 + bw - r, y0 + bh - r, r, 0.0, 0.5 * PI);
        cr.arc(x0 + r, y0 + bh - r, r, 0.5 * PI, PI);
        cr.close_path();
        cr.set_source_rgb(0.34, 0.34, 0.34);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(0.95, 0.95, 0.95);
        cr.set_line_width(3.0);
        let _ = cr.stroke();
        let at = [
            (x0, y0 + 10.0),
            (x0, y0 + bh / 2.0),
            (x0, y0 + bh - 10.0),
            (x0 + bw / 2.0, y0),
            (x0 + bw / 2.0, y0 + bh),
        ];
        for (i, (x, y)) in at.iter().enumerate() {
            if mask & (1 << i) == 0 {
                continue;
            }
            for (rad, a) in [(16.0, 0.25), (11.0, 0.45)] {
                cr.set_source_rgba(1.0, 1.0, 1.0, a);
                cr.arc(*x, *y, rad, 0.0, 2.0 * PI);
                let _ = cr.fill();
            }
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.arc(*x, *y, 6.0, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
        cr.set_source_rgb(1.0, 1.0, 1.0);
        match q {
            0 => text(cr, "lens blocked", w / 2.0, y0 + bh + 24.0, 17.0, 0.5),
            -1 => text_q(cr, "lens blocked", x0 + bw + side / 2.0, y0 + bh / 2.0, 17.0, 0.5, q),
            _ => text_q(cr, "lens blocked", x0 - 16.0 - (side - 16.0) / 2.0, y0 + bh / 2.0, 17.0, 0.5, q),
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

    // the wheels' layer, shown while there is a wheel
    fn update_wheels(&self) {
        let shown = {
            let st = self.st.borrow();
            st.wheel.is_some() || st.zoom_wheel_until.is_some_and(|t| Instant::now() < t)
        };
        self.wheels.set_visible(shown);
        if shown {
            self.wheels.queue_draw();
        }
    }

    fn draw_wheels(&self, cr: &cairo::Context, w: f64, h: f64) {
        let st = self.st.borrow();
        let q = self.quarter.get();
        cr.select_font_face("Adwaita Mono", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        // the exposure wheel: the value lists as ticks on an arc beside the dials, turning
        // with the (continuous) value, which sits on the pointer
        if let Some(dial) = st.wheel {
            let (pos, value, ticks): (f64, String, Vec<(f64, String)>) = match dial {
                Dial::Iso => (
                    st.iso,
                    iso_at(st.iso).to_string(),
                    ISO.iter().map(|&i| (iso_pos(i as f64), i.to_string())).collect(),
                ),
                Dial::Shutter => (
                    st.shutter,
                    fmt_secs(secs_at(st.shutter)),
                    SHUTTER.iter().map(|s| (secs_pos(shutter_secs(s)), s.to_string())).collect(),
                ),
                Dial::Ev => (st.ev, fmt_ev(ev_at(st.ev)), (-9..=9).map(|e| (ev_pos(e), fmt_ev(e))).collect()),
            };
            let r = 400.0;
            let (cx, cy) = (w - 340.0 + r, h / 2.0);
            // 0.1 rad between neighbouring list entries, as before
            let k = 0.1 * (ticks.len() - 1) as f64;
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.25);
            cr.set_line_width(2.0);
            cr.arc(cx, cy, r, PI - 0.7, PI + 0.7);
            let _ = cr.stroke();
            for (tp, l) in &ticks {
                // lower values below (the finger goes down for less light)
                let d = (tp - pos) * k;
                if d.abs() > 0.6 {
                    continue;
                }
                let a = PI - d;
                let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
                cr.set_source_rgba(1.0, 1.0, 1.0, 1.0 - d.abs() / 0.7);
                cr.arc(x, y, 3.0, 0.0, 2.0 * PI);
                let _ = cr.fill();
                if d.abs() > 0.06 {
                    text_q(cr, l, x - 18.0, y, 20.0, 1.0, q);
                }
            }
            let (x, y) = (cx - r, cy);
            cr.set_source_rgb(ACCENT.0, ACCENT.1, ACCENT.2);
            cr.arc(x, y, 5.0, 0.0, 2.0 * PI);
            let _ = cr.fill();
            text_q(cr, &value, x - 18.0, y, 34.0, 1.0, q);
        }
        // the zoom wheel: an arc of dots from 28 (bottom) to 150 mm (top), primes labelled
        if st.zoom_wheel_until.is_some_and(|t| Instant::now() < t) {
            let r = 300.0;
            let (cx, cy) = (w - 340.0 + r, h / 2.0);
            let angle = |z: f64| PI - 0.55 + 1.1 * (z / ZOOM_MIN).ln() / (ZOOM_MAX / ZOOM_MIN).ln();
            let point = |z: f64| {
                let a = angle(z);
                (cx + r * a.cos(), cy + r * a.sin())
            };
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.6);
            for k in 0..=30 {
                let z = ZOOM_MIN * (ZOOM_MAX / ZOOM_MIN).powf(k as f64 / 30.0);
                let (x, y) = point(z);
                cr.arc(x, y, 2.0, 0.0, 2.0 * PI);
                let _ = cr.fill();
            }
            for &p in PRIMES {
                let (x, y) = point(p);
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.arc(x, y, 4.0, 0.0, 2.0 * PI);
                let _ = cr.fill();
                text_q(cr, &format!("{p:.0}"), x + 12.0, y, 14.0, 0.0, q);
            }
            let (x, y) = point(st.zoom);
            cr.set_source_rgb(ACCENT.0, ACCENT.1, ACCENT.2);
            cr.arc(x, y, 7.0, 0.0, 2.0 * PI);
            let _ = cr.fill();
            text_q(cr, &format!("{:.0} mm", st.zoom), x - 18.0, y, 30.0, 1.0, q);
        }
    }

    fn draw_dial(&self, cr: &cairo::Context, w: f64, h: f64, top: bool) {
        let st = self.st.borrow();
        // auto has no top dial
        let (top_dial, bottom_dial) = st.mode.dials();
        let Some(d) = (if top { top_dial } else { Some(bottom_dial) }) else { return };
        let label = match d {
            Dial::Iso => "ISO",
            Dial::Shutter => "S",
            Dial::Ev => "EV",
        };
        let (enabled, dial) = (true, Some(d));
        let (cx, cy, r) = (w / 2.0, h / 2.0, w.min(h) / 2.0 - 2.0);
        if st.wheel.is_some() && st.wheel != dial {
            return;
        }
        if dial.is_some() && st.wheel == dial {
            cr.set_source_rgba(ACCENT.0, ACCENT.1, ACCENT.2, 0.18);
            cr.arc(cx, cy, r, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
        let alpha = if enabled { 1.0 } else { 0.35 };
        let active = dial.is_some() && st.wheel == dial;
        if active {
            cr.set_source_rgba(ACCENT.0, ACCENT.1, ACCENT.2, alpha);
        } else {
            cr.set_source_rgba(1.0, 1.0, 1.0, alpha * 0.9);
        }
        cr.set_line_width(2.0);
        cr.arc(cx, cy, r, 0.0, 2.0 * PI);
        let _ = cr.stroke();
        cr.select_font_face("Adwaita Mono", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        text(cr, label, cx, cy, 15.0, 0.5);
    }

    fn draw_shutter(&self, cr: &cairo::Context, w: f64, h: f64) {
        let busy = self.st.borrow().busy;
        let (cx, cy, r) = (w / 2.0, h / 2.0, w.min(h) / 2.0 - 2.0);
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.set_line_width(3.0);
        cr.arc(cx, cy, r, 0.0, 2.0 * PI);
        let _ = cr.stroke();
        if busy {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.3);
        }
        cr.arc(cx, cy, r - 7.0, 0.0, 2.0 * PI);
        let _ = cr.fill();
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
        eprintln!("l16-camera2: display transform: {e}");
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
                eprintln!("l16-camera2: blanking the screen: {e}");
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

fn hud_item(value: &gtk::Label, unit: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 1);
    b.add_css_class("hud-cell");
    value.add_css_class("hud-value");
    let u = gtk::Label::new(Some(unit));
    u.add_css_class("hud-unit");
    b.append(&u);
    b.append(value);
    b
}

fn build(gapp: &gtk::Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = gtk::ApplicationWindow::builder().application(gapp).title("Viewfinder").build();
    window.add_css_class("camera");

    let (pipeline, paintable) = make_pipeline();
    let bus = pipeline
        .bus()
        .expect("bus")
        .add_watch_local(|_, msg| {
            if let gst::MessageView::Error(e) = msg.view() {
                eprintln!("l16-camera2: preview: {} ({:?})", e.error(), e.debug());
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

    // left: the exposure readout
    let hud: Vec<gtk::Label> = (0..4).map(|_| gtk::Label::new(Some("–"))).collect();
    let left = gtk::Box::new(gtk::Orientation::Vertical, 16);
    left.set_size_request(96, -1);
    left.set_valign(gtk::Align::Center);
    let hud_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
    for (l, unit) in hud.iter().zip(["EV", "ISO", "SHUTTER", "MM"]) {
        // centred in the column, as the badges above: turned, filling it put the
        // readout against the screen's edge
        let r = turn(hud_item(l, unit).upcast_ref());
        r.set_halign(gtk::Align::Center);
        hud_box.append(&r);
    }
    left.append(&hud_box);

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
    let (top, shutter, bottom) = (dial(72), dial(92), dial(72));
    for d in [&top, &shutter, &bottom] {
        d.add_css_class("spin");
    }
    let mode_label = gtk::Label::new(Some("AUTO"));
    mode_label.add_css_class("mode-chip");
    let opener_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    opener_box.append(&icons::label(icons::CHEVRON_UP));
    opener_box.append(&turn(mode_label.upcast_ref()));
    let opener = gtk::Button::new();
    opener.set_child(Some(&opener_box));
    opener.add_css_class("flat-white");
    opener.set_halign(gtk::Align::Center);
    opener.set_margin_bottom(8);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 13);
    right.set_size_request(200, -1);
    let spacer = || {
        let s = gtk::Box::new(gtk::Orientation::Vertical, 0);
        s.set_vexpand(true);
        s
    };
    right.append(&thumb_box);
    right.append(&spacer());
    right.append(&top);
    right.append(&shutter);
    right.append(&bottom);
    right.append(&spacer());
    right.append(&opener);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&left);
    row.append(&frame);
    row.append(&right);

    // the toolbar: flash, timer, grid, burst, and the settings screen (the mode wheel beside it)
    let timer_btn = icons::button(icons::TIMER_OFF, "");
    let grid_btn = icons::button(icons::GRID_OFF, "");
    let hist_btn = icons::button(icons::HISTOGRAM, "");
    let burst_btn = icons::button(icons::BURST, "");
    let flash_btn = icons::button(icons::FLASH_OFF, "");
    let wb_btn = icons::button(icons::WB[0], "");
    let settings_btn = icons::button(icons::COG, "");
    let close_btn = icons::button(icons::CLOSE, "");
    let afd_btn = icons::button(icons::FOCUS_AUTO, "");
    let assist_btn = icons::button(icons::ASSIST_OFF, "");
    for b in [&timer_btn, &grid_btn, &hist_btn, &burst_btn, &flash_btn, &wb_btn, &settings_btn, &close_btn, &afd_btn, &assist_btn] {
        b.add_css_class("spin");
    }
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("toolbar");
    // the chosen buttons (layout_toolbar)
    let tools_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.append(&tools_box);
    let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    fill.set_hexpand(true);
    bar.append(&fill);
    bar.append(&settings_btn);
    bar.append(&close_btn);

    // the settings screen (OpenLight's: a list of title, explanation and value)
    let settings_list = gtk::ListBox::new();
    settings_list.set_selection_mode(gtk::SelectionMode::None);
    let settings_scroll = gtk::ScrolledWindow::new();
    settings_scroll.set_child(Some(&settings_list));
    settings_scroll.set_vexpand(true);
    settings_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    let settings_back = icons::button(icons::ARROW_LEFT, "settings");
    settings_back.add_css_class("flat-white");
    settings_back.set_halign(gtk::Align::Start);
    settings_back.set_margin_start(16);
    let settings_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    settings_box.add_css_class("settings");
    settings_box.append(&settings_back);
    settings_box.append(&settings_scroll);
    let settings_page = gtk::Overlay::new();
    settings_page.set_child(Some(&turn(settings_box.upcast_ref())));
    settings_page.set_visible(false);
    // the list a setting's value is chosen from, over the settings screen
    let chooser_title = gtk::Label::new(None);
    chooser_title.add_css_class("chooser-title");
    chooser_title.set_halign(gtk::Align::Start);
    let chooser_list = gtk::ListBox::new();
    chooser_list.set_selection_mode(gtk::SelectionMode::None);
    let chooser_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    chooser_card.add_css_class("chooser-card");
    chooser_card.set_halign(gtk::Align::Center);
    chooser_card.set_valign(gtk::Align::Center);
    chooser_card.set_size_request(420, -1);
    chooser_card.set_overflow(gtk::Overflow::Hidden);
    chooser_card.append(&chooser_title);
    chooser_card.append(&chooser_list);
    let chooser = gtk::Box::new(gtk::Orientation::Vertical, 0);
    chooser.add_css_class("chooser");
    chooser_card.set_vexpand(true);
    chooser.append(&turn(chooser_card.upcast_ref()));
    chooser.set_visible(false);
    settings_page.add_overlay(&chooser);
    // the toolbar editor: which buttons, in what order
    let editor_list = gtk::ListBox::new();
    editor_list.set_selection_mode(gtk::SelectionMode::None);
    let editor_scroll = gtk::ScrolledWindow::new();
    editor_scroll.set_child(Some(&editor_list));
    editor_scroll.set_vexpand(true);
    editor_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    let editor_back = icons::button(icons::ARROW_LEFT, "toolbar");
    editor_back.add_css_class("flat-white");
    editor_back.set_halign(gtk::Align::Start);
    editor_back.set_margin_start(16);
    let editor = gtk::Box::new(gtk::Orientation::Vertical, 0);
    editor.add_css_class("settings");
    editor.append(&editor_back);
    editor.append(&editor_scroll);
    editor.set_visible(false);
    settings_page.add_overlay(&turn(editor.upcast_ref()));
    let options_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    options_row.add_css_class("toolbar");
    options_row.add_css_class("options");
    let options = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .child(&options_row)
        .build();
    let toolbar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    toolbar_box.append(&options);
    toolbar_box.append(&bar);
    let toolbar = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .child(&toolbar_box)
        .valign(gtk::Align::End)
        .build();
    // the mode wheel: drawn down the whole right edge, but touched only around its labels, so
    // taps elsewhere (the toolbar) pass through
    let mode_wheel = gtk::DrawingArea::new();
    mode_wheel.set_halign(gtk::Align::End);
    mode_wheel.set_size_request(386, -1); // stock's 225 dp
    mode_wheel.set_can_target(false);
    mode_wheel.set_visible(false);
    let mode_touch = gtk::Box::new(gtk::Orientation::Vertical, 0);
    mode_touch.set_halign(gtk::Align::End);
    mode_touch.set_valign(gtk::Align::Center);
    mode_touch.set_size_request(386, MODE_TOUCH_H);
    mode_touch.set_visible(false);

    // shown only while a wheel is (exposure, or zoom just changed)
    let wheels = Canvas::new();
    wheels.set_can_target(false);
    wheels.set_visible(false);
    wheels.set_halign(gtk::Align::End);
    wheels.set_size_request(560, -1);
    let status = gtk::Label::new(None);
    status.add_css_class("status");
    status.set_valign(gtk::Align::Start);
    status.set_halign(gtk::Align::Center);
    status.set_margin_top(14);
    status.set_visible(false);
    status.set_can_target(false);
    let status_turn = turn(status.upcast_ref());
    let zoom_chips: Vec<gtk::Button> = PRIMES
        .iter()
        .map(|p| {
            let b = gtk::Button::with_label(&format!("{p:.0}"));
            b.add_css_class("zoom-chip");
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
    zoom_pill.set_margin_bottom(14);
    let zoom_turn = turn(zoom_pill.upcast_ref());
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
    let lens_badge = gtk::DrawingArea::new();
    lens_badge.set_size_request(230, 150);
    // centred in the preview (the status line keeps to its top edge)
    lens_badge.set_halign(gtk::Align::Center);
    lens_badge.set_valign(gtk::Align::Center);
    lens_badge.set_can_target(false);
    lens_badge.set_visible(false);
    // (not turned: draw_lens_blocked turns only its words)

    // stock's device status: captures left and the battery, top left
    let storage_label = gtk::Label::new(None);
    let battery_label = gtk::Label::new(None);
    storage_label.set_halign(gtk::Align::Start);
    battery_label.set_halign(gtk::Align::Start);
    let status_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    status_box.add_css_class("device-status");
    status_box.append(&storage_label);
    status_box.append(&battery_label);
    status_box.set_halign(gtk::Align::Start);
    status_box.set_valign(gtk::Align::Start);
    status_box.set_margin_start(12);
    status_box.set_margin_top(16);
    status_box.set_can_target(false);
    let status_box_turn = turn(status_box.upcast_ref());
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
    thermal_warning.set_margin_bottom(24);
    thermal_warning.set_can_target(false);
    thermal_warning.set_visible(false);
    let thermal_turn = turn(thermal_warning.upcast_ref());
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
    root.set_child(Some(&row));
    preview.add_overlay(&lens_badge);
    // on the preview, at its bottom edge as the camera is held (place_status)
    preview.add_overlay(&thermal_turn);
    preview.add_overlay(&zoom_turn);
    root.add_overlay(&status_box_turn);
    root.add_overlay(&wheels);
    preview.add_overlay(&status_turn);
    root.add_overlay(&countdown);
    root.add_overlay(&toolbar);
    root.add_overlay(&mode_wheel);
    root.add_overlay(&mode_touch);
    root.add_overlay(&burst_screen);
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
            mode_pos: 0.0,
            mode_start: 0.0,
            mode_swiped: false,
            iso: 1.0,
            ev: 0.5,
            shutter: secs_pos(1.0 / 60.0),
            zoom: ZOOM_MIN,
            module: 0,
            timer: 0,
            grid: 0,
            histogram: false,
            assist: 0,
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
        lens_badge,
        status_box,
        light: light_proxy(),
        accel: accel_proxy(),
        quarter: Cell::new(0),
        status_turn: status_turn.clone(),
        thermal_turn: thermal_turn.clone(),
        zoom_chips: zoom_chips.clone(),
        zoom_turn: zoom_turn.clone(),
        idle_cookie: Cell::new(0),
        landscape_held: RefCell::new(None),
        rotators,
        geo: RefCell::new(geo::Geo::default()),
        storage_label,
        battery_label,
        battery_screen,
        thermal_warning,
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
        hud,
        top,
        bottom,
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
        mode_label,
        toolbar,
        options,
        options_row,
        options_for: Cell::new(None),
        preview_gain: Cell::new(1.0),
        right,
        mode_wheel,
        mode_touch,
        timer_btn,
        grid_btn,
        hist_btn,
        hist: RefCell::new(vec![0; HIST_BINS * 4]),
        burst_btn,
        flash_btn,
        wb_btn,
        afd_btn,
        assist_btn,
        tools_box,
        settings_list,
        setting_taps: RefCell::new(Vec::new()),
        chooser: chooser.clone(),
        chooser_title,
        chooser_list: chooser_list.clone(),
        chooser_pick: RefCell::new(None),
        editor,
        editor_list,
        cal: wb::Calibration::load(),
        motor: haptics::Haptics::open(),
        photo_args: RefCell::new(HashMap::new()),
        hud_box,
        settings_page,
        last_saved: RefCell::new(String::new()),
        sleep_inhibitor: RefCell::new(None),
        status,
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
    app.wheels.set_draw_func(move |_, cr, w, h| a.draw_wheels(cr, w as f64, h as f64));

    let a = app.clone();
    app.lens_badge.set_draw_func(move |_, cr, w, h| a.draw_lens_blocked(cr, w as f64, h as f64));
    let a = app.clone();
    app.top.set_draw_func(move |_, cr, w, h| a.draw_dial(cr, w as f64, h as f64, true));
    let a = app.clone();
    app.bottom.set_draw_func(move |_, cr, w, h| a.draw_dial(cr, w as f64, h as f64, false));
    let a = app.clone();
    app.shutter.set_draw_func(move |_, cr, w, h| a.draw_shutter(cr, w as f64, h as f64));
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
        if a.toolbar.reveals_child() {
            a.show_toolbar(false);
        } else {
            a.focus(Some((x, y)));
        }
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
            a.set_zoom(z * 2.3f64.powf(-dy / 400.0));
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
        a.set_zoom(z * s);
    });
    app.view.add_controller(pinch);

    // the dials: tap and drag (which dial is which depends on the mode)
    for (widget, top) in [(app.top.clone(), true), (app.bottom.clone(), false)] {
        let drag = gtk::GestureDrag::new();
        let a = app.clone();
        drag.connect_drag_begin(move |_, _, _| {
            let mut st = a.st.borrow_mut();
            let (top_dial, bottom_dial) = st.mode.dials();
            let Some(dial) = (if top { top_dial } else { Some(bottom_dial) }) else { return };
            if let Some(id) = st.wheel_close.take() {
                id.remove();
            }
            st.wheel = Some(dial);
            st.wheel_start = match dial {
                Dial::Iso => st.iso,
                Dial::Shutter => st.shutter,
                Dial::Ev => st.ev,
            };
            drop(st);
            a.buzz(15);
            a.refresh();
            a.update_wheels();
        });
        let a = app.clone();
        drag.connect_drag_update(move |_, _, dy| {
            let (dial, start, dir) = {
                let st = a.st.borrow();
                (st.wheel, st.wheel_start, if st.inverse_wheel { -1.0 } else { 1.0 })
            };
            if let Some(dial) = dial {
                a.set_dial(dial, start + dir * dy * 0.001);
            }
        });
        let a = app.clone();
        drag.connect_drag_end(move |_, _, _| {
            if a.st.borrow().wheel.is_none() {
                return;
            }
            a.buzz(10);
            let b = a.clone();
            let id = glib::timeout_add_local_once(Duration::from_millis(600), move || {
                let mut st = b.st.borrow_mut();
                st.wheel_close = None;
                st.wheel = None;
                drop(st);
                b.refresh();
                b.update_wheels();
            });
            if let Some(old) = a.st.borrow_mut().wheel_close.replace(id) {
                old.remove();
            }
        });
        widget.add_controller(drag);
    }

    let click = gtk::GestureClick::new();
    let a = app.clone();
    click.connect_released(move |_, _, _, _| a.shutter_pressed());
    app.shutter.add_controller(click);

    // toolbar
    let a = app.clone();
    opener.connect_clicked(move |_| {
        let open = a.toolbar.reveals_child();
        a.show_toolbar(!open);
    });
    let a = app.clone();
    close_btn.connect_clicked(move |_| a.show_toolbar(false));
    // the mode wheel: drag it up and down (a mode per ~70 px, as stock's 0.002 of its pixels), or
    // tap a mode; it settles on the mode when let go
    let a = app.clone();
    app.mode_wheel.set_draw_func(move |_, cr, w, h| a.draw_mode_wheel(cr, w as f64, h as f64));
    let drag = gtk::GestureDrag::new();
    let a = app.clone();
    drag.connect_drag_begin(move |_, _, _| {
        let mut st = a.st.borrow_mut();
        st.mode_start = st.mode.index() as f64 / (MODES.len() - 1) as f64;
        st.mode_swiped = false;
    });
    let a = app.clone();
    drag.connect_drag_update(move |_, _, dy| {
        if dy.abs() > 8.0 {
            a.st.borrow_mut().mode_swiped = true;
        }
        let start = a.st.borrow().mode_start;
        a.set_mode_pos(start + dy * 0.0035, true);
    });
    let a = app.clone();
    drag.connect_drag_end(move |_, _, _| {
        // settle on the chosen mode (already applied as the wheel passed it)
        let pos = a.st.borrow().mode.index() as f64 / (MODES.len() - 1) as f64;
        a.set_mode_pos(pos, false);
    });
    app.mode_touch.add_controller(drag);
    let click = gtk::GestureClick::new();
    let a = app.clone();
    click.connect_released(move |_, _, _, y| {
        // the end of a swipe is not a tap on the label under the finger
        if a.st.borrow().mode_swiped {
            return;
        }
        let (pos, h) = (a.st.borrow().mode_pos, a.mode_wheel.height() as f64);
        // the touch band sits centred on the wheel
        let y = y + (h - MODE_TOUCH_H as f64) / 2.0;
        let near = (0..MODES.len())
            .map(|i| (i, (App::mode_item_y(pos, i, h, a.mode_step()) - y).abs()))
            .min_by(|p, q| p.1.total_cmp(&q.1));
        if let Some((i, d)) = near {
            if d < 30.0 {
                a.set_mode_pos(i as f64 / (MODES.len() - 1) as f64, true);
            }
        }
    });
    app.mode_touch.add_controller(click);
    // the toolbar's buttons
    for t in TOOLS {
        let a = app.clone();
        app.tool_button(t).connect_clicked(move |_| a.tool_tap(t));
    }
    app.layout_toolbar();
    let a = app.clone();
    settings_btn.connect_clicked(move |_| {
        a.show_toolbar(false);
        a.chooser.set_visible(false);
        a.editor.set_visible(false);
        a.fill_settings();
        a.settings_page.set_visible(true);
        a.follow_screen();
    });
    for (chip, &prime) in app.zoom_chips.iter().zip(PRIMES) {
        let a = app.clone();
        chip.connect_clicked(move |_| a.zoom_to(prime));
    }
    let a = app.clone();
    editor_back.connect_clicked(move |_| {
        a.editor.set_visible(false);
        a.fill_settings();
    });
    let a = app.clone();
    settings_back.connect_clicked(move |_| {
        a.settings_page.set_visible(false);
        a.follow_screen();
    });
    // back in front: the preview again at once
    let a = app.clone();
    window.connect_is_active_notify(move |w| {
        if w.is_active() {
            a.follow_screen();
        }
    });
    // the settings screen's rows, the chooser's and the editor's taps
    let a = app.clone();
    app.settings_list.connect_row_activated(move |_, r| {
        let tap = a.setting_taps.borrow().get(r.index() as usize).cloned();
        if let Some(tap) = tap {
            tap();
        }
    });
    let a = app.clone();
    app.chooser_list.connect_row_activated(move |_, r| {
        let pick = a.chooser_pick.borrow_mut().take();
        a.chooser.set_visible(false);
        if let Some(pick) = pick {
            pick(r.index() as usize);
            a.setting_changed();
            let a = a.clone();
            glib::idle_add_local_once(move || a.fill_settings());
        }
    });
    // a tap beside the list: nothing chosen
    let backdrop = gtk::GestureClick::new();
    let a = app.clone();
    let card = chooser_card.clone();
    backdrop.connect_released(move |_, _, x, y| {
        let inside = card.compute_bounds(&a.chooser).is_some_and(|b| {
            b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
        });
        if !inside {
            a.chooser_pick.borrow_mut().take();
            a.chooser.set_visible(false);
        }
    });
    app.chooser.add_controller(backdrop);

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
        eprintln!("l16-camera2: window closed");
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
                Some(&("org.l16linux.Camera2",).to_variant()),
                None,
                gtk::gio::DBusCallFlags::NONE,
                1000,
                None::<&gtk::gio::Cancellable>,
            );
            if let Err(e) = r {
                eprintln!("l16-camera2: giving up the app's name: {e}");
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
            eprintln!("l16-camera2: closed in {:.2} s", t.elapsed().as_secs_f64());
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
                eprintln!("l16-camera2: closing with {left} photo(s) still on their way");
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
        eprintln!("l16-camera2: waiting for the last camera to close");
        let mut start = Some(start);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            if !take_camera() {
                return glib::ControlFlow::Continue;
            }
            eprintln!("l16-camera2: the last camera closed");
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
                    "toolbar" => a.show_toolbar(true),
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
                        a.update_wheels();
                        a.refresh();
                    }
                    "zoom" => a.set_zoom(70.0),
                    "settings" => {
                        a.show_toolbar(false);
                        a.fill_settings();
                        a.settings_page.set_visible(true);
                    }
                    other => eprintln!("l16-camera2: no demo view {other:?}"),
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
    // (~/.cache/l16-camera2.log; appended to, as a launch that only hands over to a running
    // camera is a process too; started afresh past 1 MB)
    if unsafe { libc::isatty(2) } == 1 {
        let path = glib::user_cache_dir().join("l16-camera2.log");
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
    eprintln!("l16-camera2: started (pid {})", std::process::id());
    // GTK redraws the whole window each frame: redrawing only what changed (the preview)
    // left the badges over it as flickering black bars
    if std::env::var_os("GSK_DEBUG").is_none() {
        std::env::set_var("GSK_DEBUG", "full-redraw");
    }
    gst::init().expect("gstreamer");
    let app = gtk::Application::builder().application_id("org.l16linux.Camera2").build();
    // launched again while running (the gallery's camera button): back to the window there is
    app.connect_activate(|app| {
        // logged: an activation while the app was closing (its streams stopping) left the
        // preview dead (2026-10-02)
        eprintln!("l16-camera2: activated (window {})", app.active_window().is_some());
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

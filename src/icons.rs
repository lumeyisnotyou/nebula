// Icons: Material Design's, from the Symbols Nerd Font (font-nerd-fonts-symbols), drawn as
// text so they take the text's colour. Shared with l16-gallery.
#![allow(dead_code)]

use gtk::prelude::*;

pub const FAMILY: &str = "Symbols Nerd Font";

pub const ARROW_LEFT: char = '\u{f004d}';
pub const CAMERA: char = '\u{f0100}';
pub const CHECK: char = '\u{f012c}';
pub const CHEVRON_RIGHT: char = '\u{f0142}';
pub const CHEVRON_UP: char = '\u{f0143}';
pub const CLOSE: char = '\u{f0156}';
pub const COG: char = '\u{f0493}';
pub const DELETE: char = '\u{f01b4}';
pub const HISTOGRAM: char = '\u{f0129}';
pub const INFO: char = '\u{f02fd}';
pub const PROCESS: char = '\u{f0068}'; // auto-fix: a wand
pub const GRID: char = '\u{f02c1}';
pub const GRID_OFF: char = '\u{f02c2}';
pub const TIMER: char = '\u{f051b}';
pub const TIMER_OFF: char = '\u{f051e}';
pub const BURST: char = '\u{f0693}';
pub const FLASH: char = '\u{f0241}';
pub const FLASH_AUTO: char = '\u{f0242}';
pub const FLASH_OFF: char = '\u{f0243}';
pub const FOCUS_AUTO: char = '\u{f0f4e}';
pub const LOCK: char = '\u{f033e}';
pub const THERMOMETER: char = '\u{f050f}'; // stock's overheating warning (ic_temperature)
pub const STORAGE: char = '\u{f07dc}'; // micro-sd: captures left
pub const BATTERY_ALERT: char = '\u{f0083}';
// battery levels as stock's status icons step: >= 90, 60, 35, 15, below
pub const BATTERY: [char; 5] = ['\u{f0079}', '\u{f0080}', '\u{f007e}', '\u{f007b}', '\u{f007a}'];
pub const BATTERY_CHARGING: [char; 5] = ['\u{f0085}', '\u{f089e}', '\u{f089d}', '\u{f0086}', '\u{f089c}']; // focus locked (stock's focus_yellow_lock)
pub const CAMERA_LOCK: char = '\u{f1a15}'; // camera-lock-outline: tripod mode on
pub const HAND_WAVE: char = '\u{f1821}'; // stock's hand-shake assist ("Hold steady")
pub const MOON: char = '\u{f0594}'; // weather-night: stock's low-light assist (a stacked capture)
// focus peaking and zebras (image-filter-center-focus, and its weak variant when off)
pub const ASSIST: char = '\u{f02f1}';
pub const ASSIST_OFF: char = '\u{f02f2}';
// the modes (auto-fix, camera-iris, camera-timer, tune), metering (centre, spot, matrix), geotag, preset
// the dials: ISO (grain), shutter (a camera with a timer), exposure compensation (plus/minus)
pub const DIAL_ISO: char = '\u{f0d7c}';
pub const DIAL_SHUTTER: char = '\u{f0109}';
pub const DIAL_EV: char = '\u{f14c9}';
pub const MODES: [char; 4] = ['\u{f0068}', DIAL_ISO, '\u{f0109}', '\u{f062e}'];  // ISO priority: the dial's own ISO icon
pub const METER: [char; 3] = ['\u{f07a2}', '\u{f07a5}', '\u{f07a3}'];
pub const GEO: char = '\u{f034e}';
pub const GEO_OFF: char = '\u{f0351}';
pub const STRIP: char = '\u{f04e1}';
pub const STACK: char = '\u{f0f58}'; // layers-triple: the stack key, on
pub const STACK_OFF: char = '\u{f0f59}'; // and its outline, off
pub const CHEVRON_DOWN: char = '\u{f0140}';
// Lightbox's menu and selection
pub const MORE: char = '\u{f01d9}'; // dots-vertical
pub const COPY: char = '\u{f018f}'; // content-copy
pub const FOLDER: char = '\u{f024b}';
pub const CHECK_CIRCLE: char = '\u{f05e0}';
pub const CIRCLE_OUTLINE: char = '\u{f0130}'; // checkbox-blank-circle-outline: not chosen
pub const SELECT: char = '\u{f0139}'; // checkbox-multiple-marked-outline
// white balance presets, in wb::PRESETS' order: auto, incandescent, fluorescent, daylight, cloudy
pub const WB: [char; 5] = ['\u{f05a5}', '\u{f05a6}', '\u{f05a7}', '\u{f05a8}', '\u{f0590}'];

// Pango markup for an icon, with optional text after it
pub fn markup(icon: char, text: &str) -> String {
    let text = gtk::glib::markup_escape_text(text);
    let gap = if text.is_empty() { "" } else { " " };
    format!("<span font_family=\"{FAMILY}\" size=\"150%\">{icon}</span>{gap}{text}")
}

// a button's label as an icon (and text)
pub fn set(b: &gtk::Button, icon: char, text: &str) {
    b.set_label(&markup(icon, text));
    if let Some(l) = b.child().and_downcast::<gtk::Label>() {
        l.set_use_markup(true);
    }
}

pub fn button(icon: char, text: &str) -> gtk::Button {
    let b = gtk::Button::new();
    set(&b, icon, text);
    b
}

pub fn label(icon: char) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_markup(&markup(icon, ""));
    l
}

// a key's content: the icon over a short caption (the TE keys' legend)
pub fn set_key(b: &gtk::Button, icon: char, caption: &str) {
    let caption = gtk::glib::markup_escape_text(caption);
    b.set_label(&format!(
        "<span font_family=\"{FAMILY}\" size=\"195%\">{icon}</span>\n<span size=\"88%\" weight=\"bold\">{caption}</span>"
    ));
    if let Some(l) = b.child().and_downcast::<gtk::Label>() {
        l.set_use_markup(true);
        l.set_justify(gtk::Justification::Center);
    }
}

// The settings screen's contents: sections of rows. An on/off option is a checkbox, a choice is a
// short list of radio buttons shown in place, and a set (which functions the touch strip cycles
// through) is a list of checkboxes. A change is applied at once (App::setting_changed), and the
// pane is built again whenever its section is shown.

use crate::{App, State};
use gtk::prelude::*;
use std::rc::Rc;

pub type Get = fn(&State) -> bool;
pub type Set = fn(&mut State, bool);

pub enum Row {
    Check { title: &'static str, sub: &'static str, get: Get, set: Set },
    Radio { title: &'static str, sub: &'static str, options: &'static [&'static str], get: fn(&State) -> usize, set: fn(&mut State, usize) },
    Checks { title: &'static str, sub: &'static str, items: &'static [(&'static str, Get, Set)] },
}

pub struct Section {
    pub name: &'static str,
    pub rows: &'static [Row],
    // the About section: the app's and the system's versions, not rows
    pub about: bool,
}

pub const ACCENT_NAMES: &[&str] = &["Blue", "Orange", "Green", "Pink", "Amber", "White"];

pub static SECTIONS: &[Section] = &[
    Section {
        name: "Shooting",
        about: false,
        rows: &[
            Row::Radio {
                title: "Metering",
                sub: "Where auto exposure meters",
                options: &["Centre-weighted", "Touch", "Whole frame"],
                get: |s| s.metering as usize,
                set: |s, v| s.metering = v as u8,
            },
            Row::Check { title: "Continuous focus", sub: "Refocus when the scene changes (AF-D), outside manual mode", get: |s| s.caf, set: |s, v| s.caf = v },
            Row::Check { title: "Stacked capture", sub: "In low light, several exposures per module for less noise", get: |s| s.stacked, set: |s, v| s.stacked = v },
            Row::Radio {
                title: "Exposure steps",
                sub: "ISO and shutter in stock's 1/3 stops, or anywhere in between",
                options: &["1/3 stop", "Continuous"],
                get: |s| s.continuous as usize,
                set: |s, v| s.continuous = v == 1,
            },
        ],
    },
    Section {
        name: "Touch strip",
        about: false,
        rows: &[
            Row::Check { title: "Use the touch strip", sub: "Slide it to zoom, or to change what it is set to", get: |s| s.strip_zoom, set: |s, v| s.strip_zoom = v },
            Row::Checks {
                title: "A double tap cycles through",
                sub: "Controls the strip can adjust (ISO, shutter and EV only when the mode lets you set them)",
                items: &[
                    ("Zoom", |s| s.strip_set & 1 != 0, |s, v| if v { s.strip_set |= 1 } else { s.strip_set &= !1 }),
                    ("ISO", |s| s.strip_set & 2 != 0, |s, v| if v { s.strip_set |= 2 } else { s.strip_set &= !2 }),
                    ("Shutter", |s| s.strip_set & 4 != 0, |s, v| if v { s.strip_set |= 4 } else { s.strip_set &= !4 }),
                    ("EV", |s| s.strip_set & 8 != 0, |s, v| if v { s.strip_set |= 8 } else { s.strip_set &= !8 }),
                ],
            },
            Row::Check { title: "Reverse the direction", sub: "Slide the other way for more", get: |s| s.inverse_wheel, set: |s, v| s.inverse_wheel = v },
        ],
    },
    Section {
        name: "Display",
        about: false,
        rows: &[
            Row::Radio {
                title: "Accent colour",
                sub: "The colour of what you have set: values, selected keys, the dials",
                options: ACCENT_NAMES,
                get: |s| s.accent,
                set: |s, v| s.accent = v,
            },
            Row::Radio {
                title: "High contrast",
                sub: "Brighter panels and text for bright light; Auto uses the ambient-light sensor",
                options: &["Auto", "On", "Off"],
                get: |s| s.contrast,
                set: |s, v| s.contrast = v,
            },
            Row::Check { title: "Device status", sub: "Captures left and the battery in the corner of the viewfinder", get: |s| s.device_status, set: |s, v| s.device_status = v },
        ],
    },
    Section {
        name: "Device",
        about: false,
        rows: &[
            Row::Radio {
                title: "Haptics",
                sub: "Vibration as the controls and the zoom move",
                options: &["Off", "Normal", "Strong"],
                get: |s| s.haptics as usize,
                set: |s, v| s.haptics = v as u8,
            },
            Row::Check { title: "Geotagging", sub: "Record where photos are taken (the camera's GPS, through location services)", get: |s| s.geotag, set: |s, v| s.geotag = v },
            Row::Check { title: "Shutter sparkle", sub: "A beat of light from the LED by the shutter button when a photo is taken", get: |s| s.sparkle, set: |s, v| s.sparkle = v },
            Row::Radio {
                title: "Lens-blocked warning",
                sub: "When a finger or a case covers a lens",
                options: &["Off", "Warning", "Warning and buzz"],
                get: |s| s.lens_warning as usize,
                set: |s, v| s.lens_warning = v as u8,
            },
            Row::Check { title: "Pocket detection", sub: "Sleep after 30 s with the lenses covered in the dark", get: |s| s.pocket, set: |s, v| s.pocket = v },
        ],
    },
    Section { name: "About", about: true, rows: &[] },
];

fn text_box(title: &str, sub: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let t = gtk::Label::new(Some(title));
    t.add_css_class("set-title");
    t.set_xalign(0.0);
    b.append(&t);
    if !sub.is_empty() {
        let d = gtk::Label::new(Some(sub));
        d.add_css_class("set-sub");
        d.set_xalign(0.0);
        d.set_wrap(true);
        d.set_max_width_chars(60);
        b.append(&d);
    }
    b
}

// a checkbox with @label, set to @active, calling @on on a change
pub fn check(label: &str, active: bool, on: impl Fn(bool) + 'static) -> gtk::CheckButton {
    let l = gtk::Label::new(Some(label));
    l.add_css_class("check-label");
    let c = gtk::CheckButton::new();
    c.set_child(Some(&l));
    c.set_active(active);
    c.connect_toggled(move |c| on(c.is_active()));
    c
}

// the pane for section @index: its rows, built from the current state
pub fn fill_pane(app: &Rc<App>, pane: &gtk::Box, index: usize) {
    while let Some(c) = pane.first_child() {
        pane.remove(&c);
    }
    let section = &SECTIONS[index];
    let heading = gtk::Label::new(Some(&section.name.to_uppercase()));
    heading.add_css_class("pane-title");
    heading.set_xalign(0.0);
    pane.append(&heading);
    if section.about {
        for (k, line) in app.about_lines().iter().enumerate() {
            let l = gtk::Label::new(Some(line));
            l.add_css_class(if k == 0 { "set-title" } else { "about-line" });
            l.set_xalign(0.0);
            pane.append(&l);
        }
        return;
    }
    for row in section.rows {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
        card.add_css_class("pane-row");
        match row {
            Row::Check { title, sub, get, set } => {
                let a = app.clone();
                let c = gtk::CheckButton::new();
                c.set_child(Some(&text_box(title, sub)));
                c.set_active(get(&app.st.borrow()));
                let set = *set;
                c.connect_toggled(move |c| {
                    set(&mut a.st.borrow_mut(), c.is_active());
                    a.setting_changed();
                });
                card.append(&c);
            }
            Row::Radio { title, sub, options, get, set } => {
                card.append(&text_box(title, sub));
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 22);
                line.add_css_class("choices");
                let now = get(&app.st.borrow());
                let mut first: Option<gtk::CheckButton> = None;
                for (k, name) in options.iter().enumerate() {
                    let l = gtk::Label::new(Some(name));
                    l.add_css_class("check-label");
                    let r = gtk::CheckButton::new();
                    r.set_child(Some(&l));
                    match &first {
                        Some(f) => r.set_group(Some(f)),
                        None => first = Some(r.clone()),
                    }
                    r.set_active(k == now);
                    let (a, set) = (app.clone(), *set);
                    r.connect_toggled(move |r| {
                        if r.is_active() {
                            set(&mut a.st.borrow_mut(), k);
                            a.setting_changed();
                        }
                    });
                    line.append(&r);
                }
                card.append(&line);
            }
            Row::Checks { title, sub, items } => {
                card.append(&text_box(title, sub));
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 22);
                line.add_css_class("choices");
                for (name, get, set) in items.iter() {
                    let (a, set) = (app.clone(), *set);
                    line.append(&check(name, get(&app.st.borrow()), move |v| {
                        set(&mut a.st.borrow_mut(), v);
                        a.setting_changed();
                    }));
                }
                card.append(&line);
            }
        }
        pane.append(&card);
    }
}

// A vertical ruler for the exposure values and the zoom: a fixed scale of ticks (the higher
// values up) and a pointer that slides along it with the finger, the value above. Drawn as render nodes (rectangles, text), so a redraw
// costs the GPU a few quads rather than the CPU a surface to paint and upload. The scale glides
// to a new value (a short ease) and asks for frames only while it is moving.

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

pub const WIDTH: i32 = 120;
const ORANGE: (f32, f32, f32) = (1.0, 0.353, 0.122); // #FF5A1F, as main.rs's ACCENT
const GLIDE_SECS: f64 = 0.05; // the ease's time constant

pub struct Tick {
    pub pos: f64,
    // a labelled tick is a major one
    pub label: Option<String>,
}

pub struct Spec {
    pub unit: &'static str,
    pub ticks: Vec<Tick>,
    // pixels per unit of position, and the sign: +1 puts a greater position lower down
    pub px_per_unit: f64,
    // the value written above the scale (the exposure values have a flyout beside their encoder)
    pub show_value: bool,
    pub dir: f64,
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Ruler {
        pub spec: RefCell<Option<Spec>>,
        pub key: Cell<u32>,
        pub target: Cell<f64>,
        pub shown: Cell<f64>,
        pub value: RefCell<String>,
        pub last_frame: Cell<i64>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Ruler {
        const NAME: &'static str = "L16Ruler";
        type Type = super::Ruler;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Ruler {
        fn dispose(&self) {
            if let Some(id) = self.tick.take() {
                id.remove();
            }
        }
    }

    impl WidgetImpl for Ruler {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (WIDTH, WIDTH, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let spec = self.spec.borrow();
            let Some(spec) = spec.as_ref() else { return };
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let rgba = |r: f32, g: f32, b: f32, a: f32| gdk::RGBA::new(r, g, b, a);
            let orange = |a: f32| rgba(ORANGE.0, ORANGE.1, ORANGE.2, a);
            let white = |a: f32| rgba(0.95, 0.95, 0.93, a);
            let panel = gsk::RoundedRect::from_rect(graphene::Rect::new(0.0, 0.0, w, h), 10.0);
            snapshot.push_rounded_clip(&panel);
            snapshot.append_color(&rgba(0.055, 0.055, 0.063, 0.82), &graphene::Rect::new(0.0, 0.0, w, h));
            let (cy, shown) = (h / 2.0, self.shown.get());
            // text centred on (x, y)
            let text = |s: &str, x: f32, y: f32, size: f64, colour: &gdk::RGBA| {
                let layout = widget.create_pango_layout(Some(s));
                let mut font = pango::FontDescription::from_string("Adwaita Mono, Droid Sans Mono, Monospace Bold");
                font.set_absolute_size(size * pango::SCALE as f64);
                layout.set_font_description(Some(&font));
                let (tw, th) = layout.pixel_size();
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x - tw as f32 / 2.0, y - th as f32 / 2.0));
                snapshot.append_layout(&layout, colour);
                snapshot.restore();
            };
            // the ticks, from the panel's right edge inwards, fading out towards both ends
            let top = 76.0; // under the value
            for t in &spec.ticks {
                let y = cy + (spec.dir * (t.pos - shown) * spec.px_per_unit) as f32;
                let fade = ((y - top).min(h - 8.0 - y) / 70.0).clamp(0.0, 1.0);
                if fade <= 0.0 {
                    continue;
                }
                let major = t.label.is_some();
                let len = if major { 26.0 } else { 12.0 };
                let near = (y - cy).abs() < 3.0;
                let colour = if near { orange(fade) } else { white(fade * if major { 0.75 } else { 0.38 }) };
                snapshot.append_color(&colour, &graphene::Rect::new(w - 16.0 - len, y - 0.75, len, 1.5));
                if let Some(l) = &t.label {
                    text(l, 40.0, y, 11.0, &white(fade * if near { 1.0 } else { 0.6 }));
                }
            }
            // the pointer across the scale, the value above it and the unit under that
            snapshot.append_color(&orange(1.0), &graphene::Rect::new(w - 62.0, cy - 1.5, 52.0, 3.0));
            if spec.show_value {
                text(&self.value.borrow(), w / 2.0, 26.0, 26.0, &orange(1.0));
                text(spec.unit, w / 2.0, 52.0, 11.0, &white(0.5));
            } else {
                text(spec.unit, w / 2.0, 30.0, 12.0, &white(0.6));
            }
            snapshot.pop();
            snapshot.append_border(&panel, &[1.0; 4], &[white(0.10), white(0.10), white(0.10), white(0.10)]);
        }
    }
}

glib::wrapper! {
    pub struct Ruler(ObjectSubclass<imp::Ruler>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Ruler {
    pub fn new() -> Self {
        glib::Object::new()
    }

    // the scale for @key, built only when it changes (then the scale jumps rather than glides)
    pub fn configure(&self, key: u32, make: impl FnOnce() -> Spec) {
        let imp = self.imp();
        if imp.key.get() == key && imp.spec.borrow().is_some() {
            return;
        }
        imp.key.set(key);
        imp.spec.replace(Some(make()));
        imp.shown.set(imp.target.get());
        self.queue_draw();
    }

    // to @pos, showing @value above the pointer
    pub fn set(&self, pos: f64, value: &str) {
        let imp = self.imp();
        imp.target.set(pos);
        if *imp.value.borrow() != value {
            imp.value.replace(value.to_string());
            self.queue_draw();
        }
        self.glide();
    }

    // without the glide: for a scale that has just appeared
    pub fn jump(&self) {
        let imp = self.imp();
        imp.shown.set(imp.target.get());
        self.queue_draw();
    }

    fn glide(&self) {
        let imp = self.imp();
        if imp.tick.borrow().is_some() || self.moved_px() < 0.1 {
            return;
        }
        imp.last_frame.set(0);
        let id = self.add_tick_callback(|w, clock| {
            let imp = w.imp();
            // the time since the last frame, so the glide takes as long at any frame rate
            let now = clock.frame_time();
            let before = imp.last_frame.replace(now);
            let dt = if before == 0 { 0.016 } else { ((now - before) as f64 / 1e6).clamp(0.001, 0.05) };
            let (shown, target) = (imp.shown.get(), imp.target.get());
            let step = 1.0 - (-dt / GLIDE_SECS).exp();
            imp.shown.set(shown + (target - shown) * step);
            if w.moved_px() < 0.1 {
                imp.shown.set(target);
                imp.tick.replace(None);
                w.queue_draw();
                return glib::ControlFlow::Break;
            }
            w.queue_draw();
            glib::ControlFlow::Continue
        });
        imp.tick.replace(Some(id));
    }

    // how far the scale has still to go, in pixels
    fn moved_px(&self) -> f64 {
        let imp = self.imp();
        let px = imp.spec.borrow().as_ref().map_or(0.0, |s| s.px_per_unit);
        (imp.target.get() - imp.shown.get()).abs() * px
    }
}

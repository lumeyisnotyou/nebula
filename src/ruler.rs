// The number line under the dot-matrix value in the adjust panel: a subtle scale of ticks that
// slides under a fixed pointer, so the exact stops can be read as the value changes. Drawn as
// render nodes (rectangles, text): a redraw costs the GPU a few quads, not the CPU a surface to
// paint and upload. The scale glides to a new value (a short ease) and asks for frames only
// while it is moving.

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

pub const HEIGHT: i32 = 48;
const GLIDE_SECS: f64 = 0.05; // the ease's time constant

pub struct Tick {
    pub pos: f64,
    // a labelled tick is a major one
    pub label: Option<String>,
}

pub struct Spec {
    pub ticks: Vec<Tick>,
    // pixels per unit of position, and the sign: +1 puts a lower position to the right
    pub px_per_unit: f64,
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
                gtk::Orientation::Horizontal => (0, 0, -1, -1),
                _ => (HEIGHT, HEIGHT, -1, -1),
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
            let (ar, ag, ab) = crate::accent();
            let accent = |a: f32| rgba(ar as f32, ag as f32, ab as f32, a);
            let white = |a: f32| rgba(0.95, 0.95, 0.93, a);
            let (cx, shown) = (w / 2.0, self.shown.get());
            let text = |s: &str, x: f32, y: f32, size: f64, colour: &gdk::RGBA| {
                let layout = widget.create_pango_layout(Some(s));
                let mut font = pango::FontDescription::from_string("DM Mono, Adwaita Mono, Droid Sans Mono, Monospace Medium");
                font.set_absolute_size(size * crate::TEXT_SCALE * pango::SCALE as f64);
                layout.set_font_description(Some(&font));
                let (tw, th) = layout.pixel_size();
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x - tw as f32 / 2.0, y - th as f32 / 2.0));
                snapshot.append_layout(&layout, colour);
                snapshot.restore();
            };
            // the ticks, fading out towards both ends
            for t in &spec.ticks {
                let x = cx + (spec.dir * (shown - t.pos) * spec.px_per_unit) as f32;
                let fade = ((w / 2.0 - (x - cx).abs()) / 50.0).clamp(0.0, 1.0);
                if fade <= 0.0 {
                    continue;
                }
                let major = t.label.is_some();
                let len = if major { 14.0 } else { 7.0 };
                let near = (x - cx).abs() < 2.0;
                let colour = if near { accent(fade) } else { white(fade * if major { 0.42 } else { 0.2 }) };
                snapshot.append_color(&colour, &graphene::Rect::new(x - 0.5, 4.0, 1.0, len));
                if let Some(l) = &t.label {
                    text(l, x, h - 11.0, 12.5, &white(fade * if near { 0.9 } else { 0.38 }));
                }
            }
            // the pointer
            snapshot.append_color(&accent(1.0), &graphene::Rect::new(cx - 1.0, 2.0, 2.0, 18.0));
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

    // to @pos
    pub fn set(&self, pos: f64) {
        self.imp().target.set(pos);
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

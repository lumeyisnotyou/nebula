// The whole UI laid out at the size it was drawn for (960 x 540 logical px: the L16's 1920 x 1080
// panel at 200 %) and scaled to the window, so another display scale shows the same layout
// instead of a bigger one that overflows (250 % leaves 768 x 432 logical px). The child is
// allocated its design-size area (more along one side if the window's shape differs) under a
// scale transform; GTK draws that in vectors, so text stays sharp, and picks through it, so a
// touch lands where it looks. Everything inside keeps thinking in design pixels.

use gtk::glib;
use gtk::gsk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

// the logical size the layout was drawn for
pub const DESIGN: (f64, f64) = (960.0, 540.0);

// the scale that fits the design into @w x @h logical px (1 at the design's own size)
pub fn scale_for(w: f64, h: f64) -> f64 {
    if w <= 0.0 || h <= 0.0 {
        return 1.0;
    }
    (w / DESIGN.0).min(h / DESIGN.1).clamp(0.4, 3.0)
}

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Fit {
        pub child: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Fit {
        const NAME: &'static str = "L16Fit";
        type Type = super::Fit;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Fit {
        fn dispose(&self) {
            if let Some(c) = self.child.take() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for Fit {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        // asks for nothing: the window decides, the child adapts
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(c) = self.child.borrow().clone() else { return };
            let s = super::scale_for(width as f64, height as f64);
            if (s - 1.0).abs() < 0.001 {
                c.allocate(width, height, -1, None);
                return;
            }
            let (cw, ch) = ((width as f64 / s).round() as i32, (height as f64 / s).round() as i32);
            let t = gsk::Transform::new().scale(s as f32, s as f32);
            c.allocate(cw, ch, -1, Some(t));
        }
    }
}

glib::wrapper! {
    pub struct Fit(ObjectSubclass<imp::Fit>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Fit {
    // @child, fitted to whatever room the Fit gets
    pub fn wrap(child: &impl IsA<gtk::Widget>) -> Self {
        let f: Self = glib::Object::new();
        f.set_hexpand(true);
        f.set_vexpand(true);
        let c = child.as_ref();
        c.set_parent(&f);
        f.imp().child.replace(Some(c.clone()));
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_design_size_is_unscaled() {
        assert_eq!(scale_for(960.0, 540.0), 1.0);
    }

    #[test]
    fn two_and_a_half_times_is_four_fifths() {
        // 1920 x 1080 px at 250 %
        assert!((scale_for(768.0, 432.0) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn three_times() {
        assert!((scale_for(640.0, 360.0) - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_larger_window_scales_up() {
        // 1920 x 1080 px at 100 %
        assert_eq!(scale_for(1920.0, 1080.0), 2.0);
    }

    #[test]
    fn the_tighter_side_decides() {
        // wide but short: 768 px of height would be 0.8 * 540, so the height rules
        assert!((scale_for(1200.0, 432.0) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn nonsense_sizes_leave_it_alone() {
        assert_eq!(scale_for(0.0, 540.0), 1.0);
        assert_eq!(scale_for(960.0, -1.0), 1.0);
    }

    #[test]
    fn extremes_are_clamped() {
        assert_eq!(scale_for(10.0, 10.0), 0.4);
        assert_eq!(scale_for(100000.0, 100000.0), 3.0);
    }
}

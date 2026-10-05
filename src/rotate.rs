// A widget turned a quarter for portrait, re-laid out: stock's RotateLayout. Measured the
// other way round, allocated with width and height swapped, and turned about its centre, so
// text keeps its room upright (the HUD, warnings, the settings screen). Icons that fit their
// space either way turn in place instead (the "spin" CSS class, as stock's View.rotation).

use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Rotator {
        pub child: RefCell<Option<gtk::Widget>>,
        // quarter turns clockwise: -1, 0 or 1
        pub quarter: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Rotator {
        const NAME: &'static str = "L16Rotator";
        type Type = super::Rotator;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Rotator {
        fn dispose(&self) {
            if let Some(c) = self.child.take() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for Rotator {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(c) = self.child.borrow().clone() else { return (0, 0, -1, -1) };
            let o = if self.quarter.get() == 0 {
                orientation
            } else if orientation == gtk::Orientation::Horizontal {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            };
            let (min, nat, _, _) = c.measure(o, for_size);
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(c) = self.child.borrow().clone() else { return };
            let q = self.quarter.get();
            if q == 0 {
                c.allocate(width, height, -1, None);
                return;
            }
            let (cw, ch) = (height, width);
            let t = gsk::Transform::new()
                .translate(&graphene::Point::new(width as f32 / 2.0, height as f32 / 2.0))
                .rotate(90.0 * q as f32)
                .translate(&graphene::Point::new(-cw as f32 / 2.0, -ch as f32 / 2.0));
            c.allocate(cw, ch, -1, Some(t));
        }
    }
}

glib::wrapper! {
    pub struct Rotator(ObjectSubclass<imp::Rotator>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Rotator {
    // @child in a Rotator that takes its place: its alignment, margins, expansion and
    // touchability, and follows its visibility
    pub fn wrap(child: &impl IsA<gtk::Widget>) -> Self {
        let r: Self = glib::Object::new();
        let c = child.as_ref();
        r.set_halign(c.halign());
        r.set_valign(c.valign());
        r.set_hexpand(c.hexpands());
        r.set_vexpand(c.vexpands());
        r.set_margin_top(c.margin_top());
        r.set_margin_bottom(c.margin_bottom());
        r.set_margin_start(c.margin_start());
        r.set_margin_end(c.margin_end());
        r.set_can_target(c.can_target());
        c.set_halign(gtk::Align::Fill);
        c.set_valign(gtk::Align::Fill);
        c.set_margin_top(0);
        c.set_margin_bottom(0);
        c.set_margin_start(0);
        c.set_margin_end(0);
        c.bind_property("visible", &r, "visible").sync_create().build();
        c.set_parent(&r);
        r.imp().child.replace(Some(c.clone()));
        r
    }

    pub fn set_quarter(&self, q: i32) {
        if self.imp().quarter.replace(q) != q {
            self.queue_resize();
        }
    }
}

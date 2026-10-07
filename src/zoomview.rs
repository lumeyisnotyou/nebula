// The preview: the camera's paintable, filling the widget, cropped in by a zoom factor
// (digital zoom between the modules' focal lengths). Its digital gain is the software ISP's.
//
// Focus peaking and zebras are worked out on a small copy of the frame (OVERLAY_W wide), a few
// times a second, into a texture of their own that is drawn over the preview scaled up: the
// per-frame cost is a textured quad, not several passes at the screen's size.

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct ZoomView {
        pub paintable: RefCell<Option<gdk::Paintable>>,
        pub zoom: Cell<f64>,
        pub next_zoom: Cell<Option<f64>>,
        // focus peaking and zebras, drawn over the preview by render nodes (GPU): 0 off,
        // bit 0 peaking, bit 1 zebras
        pub assist: Cell<u8>,
        // the composition grid: 0 off, 1 thirds, 2 the golden ratio's lines (rectangles, not a texture:
        // nothing to draw again when the preview is resized)
        pub grid: Cell<u8>,
        // the assist's overlay, as of its last refresh, and the timer that makes it
        pub overlay: RefCell<Option<gdk::Texture>>,
        pub overlay_timer: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ZoomView {
        const NAME: &'static str = "L16ZoomView";
        type Type = super::ZoomView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ZoomView {}

    impl WidgetImpl for ZoomView {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f64, widget.height() as f64);
            let Some(p) = self.paintable.borrow().clone() else { return };
            let ar = p.intrinsic_aspect_ratio();
            if ar <= 0.0 || w <= 0.0 || h <= 0.0 {
                return;
            }
            // cover the widget, then crop in
            let (mut pw, mut ph) = if w / h > ar { (w, w / ar) } else { (h * ar, h) };
            // libcamera's software ISP leaves its last column blue: push a source pixel off
            // each edge
            let src_w = p.intrinsic_width().max(3) as f64;
            let z = self.zoom.get().max(1.0) * src_w / (src_w - 2.0);
            pw *= z;
            ph *= z;
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
            snapshot.save();
            snapshot.translate(&graphene::Point::new(((w - pw) / 2.0) as f32, ((h - ph) / 2.0) as f32));
            p.snapshot(snapshot, pw, ph);
            if self.assist.get() != 0 {
                if let Some(t) = self.overlay.borrow().as_ref() {
                    snapshot.append_scaled_texture(t, gsk::ScalingFilter::Linear, &graphene::Rect::new(0.0, 0.0, pw as f32, ph as f32));
                }
            }
            snapshot.restore();
            snapshot.pop();
            let g = self.grid.get();
            if g != 0 {
                let at = if g == 1 { [1.0 / 3.0, 2.0 / 3.0] } else { [0.382, 0.618] };
                let c = gdk::RGBA::new(1.0, 1.0, 1.0, 0.4);
                for f in at {
                    let (x, y) = ((w * f).round() as f32, (h * f).round() as f32);
                    snapshot.append_color(&c, &graphene::Rect::new(x, 0.0, 1.0, h as f32));
                    snapshot.append_color(&c, &graphene::Rect::new(0.0, y, w as f32, 1.0));
                }
            }
        }
    }
}

const OVERLAY_W: f64 = 360.0;
const OVERLAY_EVERY: u64 = 110; // ms

// The colour matrix node clamps what it makes to 0..1, so a steep gain with an offset is a
// threshold: bright (or, below, edgy) enough is white, the rest black. The same weight for
// every channel, so how the matrix is laid out doesn't matter.
fn threshold(snapshot: &gtk::Snapshot, gain: f32, at: f32) {
    let g = gain / 3.0;
    let matrix = graphene::Matrix::from_float([
        g, g, g, 0.0, //
        g, g, g, 0.0, //
        g, g, g, 0.0, //
        0.0, 0.0, 0.0, 0.0,
    ]);
    snapshot.push_color_matrix(&matrix, &graphene::Vec4::new(-at * gain, -at * gain, -at * gain, 1.0));
}

// Focus peaking: where the image differs from a blurred copy of itself (fine detail, which
// is what focus is), green. A mask: first the mask's image, then what the mask shows.
fn peaking(snapshot: &gtk::Snapshot, p: &gdk::Paintable, rect: &graphene::Rect) {
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    snapshot.push_mask(gsk::MaskMode::Luminance);
    threshold(snapshot, 16.0, 0.022);
    snapshot.push_blend(gsk::BlendMode::Difference);
    p.snapshot(snapshot, w, h);
    snapshot.pop();
    snapshot.push_blur(0.8);
    p.snapshot(snapshot, w, h);
    snapshot.pop();
    snapshot.pop(); // the blend
    snapshot.pop(); // the colour matrix
    snapshot.pop(); // the mask's image
    snapshot.append_color(&gdk::RGBA::new(0.22, 1.0, 0.40, 0.95), rect);
    snapshot.pop(); // the mask
}

// Zebras: stripes over what is about to clip (a luma over 94 %).
fn zebras(snapshot: &gtk::Snapshot, p: &gdk::Paintable, rect: &graphene::Rect) {
    snapshot.push_mask(gsk::MaskMode::Luminance);
    threshold(snapshot, 40.0, 0.94);
    p.snapshot(snapshot, rect.width() as f64, rect.height() as f64);
    snapshot.pop(); // the colour matrix
    snapshot.pop(); // the mask's image
    let (red, clear) = (gdk::RGBA::new(1.0, 0.25, 0.25, 0.8), gdk::RGBA::new(1.0, 0.25, 0.25, 0.0));
    snapshot.append_repeating_linear_gradient(
        rect,
        &graphene::Point::new(0.0, 0.0),
        &graphene::Point::new(5.0, 5.0),
        &[
            gsk::ColorStop::new(0.0, red),
            gsk::ColorStop::new(0.5, red),
            gsk::ColorStop::new(0.5, clear),
            gsk::ColorStop::new(1.0, clear),
        ],
    );
    snapshot.pop(); // the mask
}

glib::wrapper! {
    pub struct ZoomView(ObjectSubclass<imp::ZoomView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ZoomView {
    pub fn new(paintable: &gdk::Paintable) -> Self {
        let view: ZoomView = glib::Object::new();
        view.imp().zoom.set(1.0);
        view.imp().paintable.replace(Some(paintable.clone()));
        let weak = view.downgrade();
        paintable.connect_invalidate_contents(move |_| {
            if let Some(v) = weak.upgrade() {
                if let Some(z) = v.imp().next_zoom.take() {
                    v.imp().zoom.set(z);
                }
                v.queue_draw();
            }
        });
        view
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.imp().next_zoom.set(None);
        self.imp().zoom.set(zoom);
        self.queue_draw();
    }

    pub fn set_grid(&self, grid: u8) {
        if self.imp().grid.replace(grid) != grid {
            self.queue_draw();
        }
    }

    pub fn set_assist(&self, assist: u8) {
        let imp = self.imp();
        if imp.assist.replace(assist) == assist {
            return;
        }
        if assist == 0 {
            imp.overlay.replace(None);
            if let Some(id) = imp.overlay_timer.take() {
                id.remove();
            }
        } else if imp.overlay_timer.borrow().is_none() {
            let weak = self.downgrade();
            let id = glib::timeout_add_local(std::time::Duration::from_millis(OVERLAY_EVERY), move || {
                let Some(v) = weak.upgrade() else { return glib::ControlFlow::Break };
                if v.imp().assist.get() == 0 {
                    v.imp().overlay_timer.replace(None);
                    return glib::ControlFlow::Break;
                }
                v.refresh_overlay();
                glib::ControlFlow::Continue
            });
            imp.overlay_timer.replace(Some(id));
            self.refresh_overlay();
        }
        self.queue_draw();
    }

    // the overlay again, from the preview's latest frame, at a small size
    fn refresh_overlay(&self) {
        let imp = self.imp();
        let Some(p) = imp.paintable.borrow().clone() else { return };
        let ar = p.intrinsic_aspect_ratio();
        let Some(renderer) = self.native().and_then(|n| n.renderer()) else { return };
        if ar <= 0.0 {
            return;
        }
        let rect = graphene::Rect::new(0.0, 0.0, OVERLAY_W as f32, (OVERLAY_W / ar) as f32);
        let snapshot = gtk::Snapshot::new();
        // (not to the edge: the blur there sees nothing beyond, and would outline it)
        snapshot.push_clip(&rect.inset_r(3.0, 3.0));
        let assist = imp.assist.get();
        if assist & 1 != 0 {
            peaking(&snapshot, &p, &rect);
        }
        if assist & 2 != 0 {
            zebras(&snapshot, &p, &rect);
        }
        snapshot.pop();
        let Some(node) = snapshot.to_node() else { return };
        imp.overlay.replace(Some(renderer.render_texture(&node, Some(&rect))));
        self.queue_draw();
    }

    pub fn zoom(&self) -> f64 {
        self.imp().zoom.get().max(1.0)
    }

    // from the next frame on (the first of another module)
    pub fn set_zoom_next_frame(&self, zoom: f64) {
        self.imp().next_zoom.set(Some(zoom));
    }
}

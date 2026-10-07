// A Cairo drawing kept as a texture. GTK draws a DrawingArea's Cairo node again on every frame
// it renders, and with the preview it renders the window 30 times a second; a texture stays on
// the GPU. The drawing is made again only when asked (queue_draw) or when the widget's size or
// the screen's scale changes. Sized by the widget, not the texture (the screen's scale is
// fractional: 1.75).

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::cairo;

type DrawFn = Box<dyn Fn(&cairo::Context, i32, i32)>;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Canvas {
        pub draw: RefCell<Option<DrawFn>>,
        pub texture: RefCell<Option<gdk::Texture>>,
        // what the texture was made for: width, height, scale
        pub made_for: Cell<(i32, i32, f64)>,
        pub dirty: Cell<bool>,
        // while the widget is being resized (an animation) the last texture is drawn stretched, and
        // the drawing is made again for the new size once it has stayed put: a Canvas the size of the
        // preview made again on every frame cost 30-80 ms of each (the stow animation hitched)
        pub resized_at: Cell<Option<std::time::Instant>>,
        pub settle: RefCell<Option<glib::SourceId>>,
        pub force: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "L16Canvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {}

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width(), widget.height());
            if w <= 0 || h <= 0 {
                return;
            }
            let scale = widget
                .native()
                .and_then(|n| n.surface())
                .map_or(widget.scale_factor() as f64, |s| s.scale());
            let mut render = self.dirty.replace(false) || self.force.replace(false);
            if self.made_for.get() != (w, h, scale) {
                if self.texture.borrow().is_none() {
                    render = true;
                } else {
                    self.resized_at.set(Some(std::time::Instant::now()));
                    if self.settle.borrow().is_none() {
                        let weak = widget.downgrade();
                        let id = glib::timeout_add_local(std::time::Duration::from_millis(60), move || {
                            let Some(c) = weak.upgrade() else { return glib::ControlFlow::Break };
                            let imp = c.imp();
                            let still = imp.resized_at.get().is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(100));
                            if still {
                                return glib::ControlFlow::Continue;
                            }
                            imp.settle.take();
                            imp.force.set(true);
                            // the texture is made for the size as it is now on the next snapshot
                            WidgetExt::queue_draw(&c);
                            glib::ControlFlow::Break
                        });
                        self.settle.replace(Some(id));
                    }
                }
            }
            if render {
                self.made_for.set((w, h, scale));
                self.texture.replace(self.render(w, h, scale));
            }
            if let Some(t) = self.texture.borrow().as_ref() {
                snapshot.append_texture(t, &graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
            }
        }
    }

    impl Canvas {
        fn render(&self, w: i32, h: i32, scale: f64) -> Option<gdk::Texture> {
            let draw = self.draw.borrow();
            let draw = draw.as_ref()?;
            let (pw, ph) = ((w as f64 * scale).ceil() as i32, (h as f64 * scale).ceil() as i32);
            let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pw, ph).ok()?;
            {
                let cr = cairo::Context::new(&surface).ok()?;
                cr.scale(pw as f64 / w as f64, ph as f64 / h as f64);
                draw(&cr, w, h);
            }
            surface.flush();
            let stride = surface.stride() as usize;
            // the texture takes the surface's pixels as they are: no copy of a whole screen
            let bytes = glib::Bytes::from_owned(surface.take_data().ok()?);
            Some(
                gdk::MemoryTexture::new(pw, ph, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, stride)
                    .upcast(),
            )
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Canvas {
    pub fn new() -> Self {
        glib::Object::new()
    }

    // as DrawingArea's (the widget, the context, the width, the height)
    pub fn set_draw_func(&self, f: impl Fn(&Canvas, &cairo::Context, i32, i32) + 'static) {
        let weak = self.downgrade();
        self.imp().draw.replace(Some(Box::new(move |cr, w, h| {
            if let Some(c) = weak.upgrade() {
                f(&c, cr, w, h);
            }
        })));
        self.queue_draw();
    }

    // the drawing made again, on the next frame
    pub fn queue_draw(&self) {
        self.imp().dirty.set(true);
        WidgetExt::queue_draw(self);
    }
}

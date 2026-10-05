// Development aids, off unless asked for:
// - L16_DEMO=1: a drawn scene in place of the camera, so the app runs on a desktop without the
//   hardware (the controls, the sensors and the driver are simply absent);
// - screenshots of the window as GTK renders it: SIGUSR1 saves one to
//   ~/.cache/l16-camera2-shot.png, and L16_SHOT=path takes one a moment after the start and
//   closes the window.

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, graphene};
use std::f64::consts::PI;
use std::path::Path;
use std::time::Duration;

pub fn demo() -> bool {
    std::env::var_os("L16_DEMO").is_some()
}

// a dusk landscape: a bright sky and a clipped sun, hard ridges, a dark foreground. Enough
// range for the histogram, and edges for peaking.
pub fn demo_paintable() -> gdk::Paintable {
    let (w, h) = (1024, 768);
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).expect("surface");
    {
        let cr = cairo::Context::new(&surface).expect("context");
        let (wf, hf) = (w as f64, h as f64);
        let sky = cairo::LinearGradient::new(0.0, 0.0, 0.0, hf * 0.7);
        sky.add_color_stop_rgb(0.0, 0.08, 0.12, 0.30);
        sky.add_color_stop_rgb(0.55, 0.85, 0.45, 0.30);
        sky.add_color_stop_rgb(1.0, 1.0, 0.82, 0.55);
        let _ = cr.set_source(&sky);
        let _ = cr.paint();
        // the sun, over-exposed at its middle
        let glow = cairo::RadialGradient::new(wf * 0.68, hf * 0.58, 0.0, wf * 0.68, hf * 0.58, hf * 0.45);
        glow.add_color_stop_rgba(0.0, 1.0, 1.0, 0.95, 1.0);
        glow.add_color_stop_rgba(0.12, 1.0, 0.95, 0.7, 1.0);
        glow.add_color_stop_rgba(1.0, 1.0, 0.6, 0.3, 0.0);
        let _ = cr.set_source(&glow);
        let _ = cr.paint();
        // ridges, nearer ones darker
        for (i, (base, amp, shade)) in [(0.62, 0.07, 0.30), (0.70, 0.09, 0.18), (0.80, 0.06, 0.08)].iter().enumerate() {
            cr.move_to(0.0, hf);
            let mut x = 0.0;
            while x <= wf {
                let y = hf * (base + amp * ((x / wf * 2.0 * PI * (2.0 + i as f64)) + i as f64 * 1.7).sin().abs());
                cr.line_to(x, y);
                x += 8.0;
            }
            cr.line_to(wf, hf);
            cr.close_path();
            cr.set_source_rgb(shade * 0.9, shade * 0.7, shade * 0.85);
            let _ = cr.fill();
        }
        // a few fine lines in the foreground: something sharp to focus on
        cr.set_source_rgb(0.02, 0.02, 0.03);
        cr.set_line_width(2.0);
        for k in 0..40 {
            let x = k as f64 * wf / 40.0;
            cr.move_to(x, hf);
            cr.line_to(x + 18.0, hf * 0.9 - (k % 5) as f64 * 14.0);
        }
        let _ = cr.stroke();
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.take_data().expect("pixels");
    gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &glib::Bytes::from_owned(data), stride)
        .upcast()
}

// the window as rendered, at the display's scale
pub fn screenshot(window: &gtk::ApplicationWindow, path: &Path) {
    let (w, h) = (window.width() as f32, window.height() as f32);
    let scale = window.scale_factor().max(1) as f32;
    let snap = gtk::Snapshot::new();
    snap.scale(scale, scale);
    gtk::WidgetPaintable::new(Some(window)).snapshot(&snap, w as f64, h as f64);
    let (Some(node), Some(renderer)) = (snap.to_node(), window.native().and_then(|n| n.renderer())) else {
        eprintln!("l16-camera2: screenshot: nothing to render");
        return;
    };
    let texture = renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, w * scale, h * scale)));
    match texture.save_to_png(path) {
        Ok(()) => eprintln!("l16-camera2: screenshot {}", path.display()),
        Err(e) => eprintln!("l16-camera2: screenshot {}: {e}", path.display()),
    }
}

pub fn hooks(window: &gtk::ApplicationWindow) {
    let w = window.clone();
    glib::unix_signal_add_local(libc::SIGUSR1, move || {
        screenshot(&w, &glib::user_cache_dir().join("l16-camera2-shot.png"));
        glib::ControlFlow::Continue
    });
    if let Some(path) = std::env::var_os("L16_SHOT") {
        let w = window.clone();
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            screenshot(&w, Path::new(&path));
            w.close();
        });
    }
}

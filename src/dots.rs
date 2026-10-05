// Dot-matrix numerals, as on a piece of TE hardware: 5x7 glyphs drawn as round dots, the lit
// ones in a colour and the others faint. For a Canvas's draw function (a small texture,
// painted again only when the value changes).

use crate::canvas::Canvas;
use gtk::cairo;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

// five bits a row, the leftmost the high bit
const GLYPHS: &[(char, [u8; 7])] = &[
    ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
    ('3', [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('4', [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100]),
    ('/', [0b00001, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b10000]),
    ('.', [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100]),
    ('-', [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000]),
    ('+', [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000]),
    ('x', [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001]),
];

// the characters the formatters make that a glyph stands in for: the thirds of a stop as a
// decimal, the dashes as a minus
fn normalise(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '⅓' => vec!['.', '3'],
            '⅔' => vec!['.', '7'],
            '–' | '−' | '—' => vec!['-'],
            c => vec![c],
        })
        .collect()
}

fn glyph(c: char) -> [u8; 7] {
    GLYPHS.iter().find(|(g, _)| *g == c).map_or([0; 7], |(_, rows)| *rows)
}

// the pitch at which @text fits @room, at most @max
pub fn fit(text: &str, room: f64, max: f64) -> f64 {
    let n = normalise(text).chars().count() as f64;
    (room / (6.0 * n - 1.0).max(1.0)).min(max)
}

// one glyph with its left at @x0 and top at @top, everything at @alpha
fn draw_glyph(cr: &cairo::Context, c: char, x0: f64, top: f64, pitch: f64, on: (f64, f64, f64, f64), alpha: f64) {
    let radius = pitch * 0.38;
    for (row, bits) in glyph(c).iter().enumerate() {
        for col in 0..5 {
            let lit = bits >> (4 - col) & 1 == 1;
            let (x, y) = (x0 + (col as f64 + 0.5) * pitch, top + (row as f64 + 0.5) * pitch);
            if lit {
                cr.set_source_rgba(on.0, on.1, on.2, on.3 * alpha);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.07 * alpha);
            }
            cr.arc(x, y, radius, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
    }
}

// A number that rolls to its new value, as SwiftUI's numeric text does: each digit that changed
// slides out in the direction the value moved (up when it rose) and fades, the new one comes in
// from the other side; the digits that did not change stay. Redrawn by a frame callback of its
// canvas for a moment after each change, and only then.
pub struct NumAnim {
    text: RefCell<String>,
    prev: RefCell<String>,
    t: Cell<f64>,
    dir: Cell<f64>,
    last_pos: Cell<f64>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    last_frame: Cell<i64>,
}

const ROLL_SECS: f64 = 0.16;

impl NumAnim {
    pub fn new() -> Rc<NumAnim> {
        Rc::new(NumAnim {
            text: RefCell::new(String::new()),
            prev: RefCell::new(String::new()),
            t: Cell::new(1.0),
            dir: Cell::new(1.0),
            last_pos: Cell::new(0.0),
            tick: RefCell::new(None),
            last_frame: Cell::new(0),
        })
    }

    // @value to show, at @pos in its range (a rise is a position that went up): rolls if it differs
    pub fn set(self: &Rc<Self>, canvas: &Canvas, value: &str, pos: f64) {
        if *self.text.borrow() == value {
            self.last_pos.set(pos);
            return;
        }
        let dir = if pos >= self.last_pos.get() { 1.0 } else { -1.0 };
        self.last_pos.set(pos);
        let first = self.text.borrow().is_empty();
        self.prev.replace(self.text.replace(value.to_string()));
        self.dir.set(dir);
        self.t.set(if first { 1.0 } else { 0.0 });
        canvas.queue_draw();
        if first || self.tick.borrow().is_some() {
            return;
        }
        self.last_frame.set(0);
        let me = self.clone();
        let id = canvas.add_tick_callback(move |w, clock| {
            let now = clock.frame_time();
            let before = me.last_frame.replace(now);
            let dt = if before == 0 { 0.016 } else { ((now - before) as f64 / 1e6).clamp(0.001, 0.05) };
            me.t.set((me.t.get() + dt / ROLL_SECS).min(1.0));
            if let Some(c) = w.downcast_ref::<Canvas>() {
                c.queue_draw();
            }
            if me.t.get() >= 1.0 {
                me.tick.replace(None);
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
        self.tick.replace(Some(id));
    }

    pub fn text(&self) -> String {
        self.text.borrow().clone()
    }

    // centred on @cx, glyph tops at @top
    pub fn draw(&self, cr: &cairo::Context, cx: f64, top: f64, pitch: f64, on: (f64, f64, f64, f64)) {
        let new = normalise(&self.text.borrow());
        let t = self.t.get();
        let old = normalise(&self.prev.borrow());
        let n = new.chars().count().max(if t < 1.0 { old.chars().count() } else { 0 });
        let pad = |s: &str| -> Vec<char> {
            let mut v: Vec<char> = s.chars().collect();
            while v.len() < n {
                v.insert(0, ' ');
            }
            v
        };
        let (new, old) = (pad(&new), pad(&old));
        let x0 = cx - ((6 * n) as f64 - 1.0) * pitch / 2.0;
        let e = 1.0 - (1.0 - t).powi(3);
        let travel = 7.0 * pitch * 0.55;
        let dir = self.dir.get();
        for i in 0..n {
            let x = x0 + (6 * i) as f64 * pitch;
            if t >= 1.0 || old[i] == new[i] {
                draw_glyph(cr, new[i], x, top, pitch, on, 1.0);
            } else {
                draw_glyph(cr, old[i], x, top - dir * e * travel, pitch, on, 1.0 - e);
                draw_glyph(cr, new[i], x, top + dir * (1.0 - e) * travel, pitch, on, e);
            }
        }
    }
}


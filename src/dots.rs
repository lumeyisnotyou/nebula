// Dot-matrix numerals, as on a piece of TE hardware: 5x7 glyphs drawn as round dots, the lit
// ones in a colour and the others faint, in a fixed number of cells, left-justified, the blank
// cells shown as unlit dots; a new value shows at once, as on real hardware. For a Canvas's draw
// function (a small texture, painted again only when the value changes).

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

// the width of @cells cells with the dots @pitch apart (a glyph is 5 dots and a gap of one)
pub fn cells_width(cells: usize, pitch: f64) -> f64 {
    (6.0 * cells as f64 - 1.0).max(0.0) * pitch
}

// one glyph with its left at @x0 and its top at @top, everything at @alpha
fn draw_glyph(cr: &cairo::Context, c: char, x0: f64, top: f64, pitch: f64, on: (f64, f64, f64, f64), alpha: f64) {
    let radius = pitch * 0.4;
    for (row, bits) in glyph(c).iter().enumerate() {
        for col in 0..5 {
            let lit = bits >> (4 - col) & 1 == 1;
            let (x, y) = (x0 + (col as f64 + 0.5) * pitch, top + (row as f64 + 0.5) * pitch);
            if lit {
                cr.set_source_rgba(on.0, on.1, on.2, on.3 * alpha);
            } else {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.09);
            }
            cr.arc(x, y, radius, 0.0, 2.0 * PI);
            let _ = cr.fill();
        }
    }
}

// A number in fixed cells (left-justified, blank cells as unlit dots) whose changed cells roll
// to the new character: the old one slides out in the direction the value moved and fades, the
// new one slides in from the other side, inside the display (clipped to the cells). The cells
// that did not change stay. Redrawn by a frame callback of its canvas for a moment after each
// change, and only then.
pub struct Roll {
    text: RefCell<String>,
    prev: RefCell<String>,
    t: Cell<f64>,
    dir: Cell<f64>,
    last_pos: Cell<f64>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    last_frame: Cell<i64>,
}

const ROLL_SECS: f64 = 0.17;

impl Roll {
    pub fn new() -> Rc<Roll> {
        Rc::new(Roll {
            text: RefCell::new(String::new()),
            prev: RefCell::new(String::new()),
            t: Cell::new(1.0),
            dir: Cell::new(1.0),
            last_pos: Cell::new(0.0),
            tick: RefCell::new(None),
            last_frame: Cell::new(0),
        })
    }

    // @value to show, with the value's @pos (a rise is a position that went up): it rolls if it differs
    pub fn set(self: &Rc<Self>, canvas: &Canvas, value: &str, pos: f64) {
        if *self.text.borrow() == value {
            self.last_pos.set(pos);
            return;
        }
        let first = self.text.borrow().is_empty();
        self.dir.set(if pos >= self.last_pos.get() { 1.0 } else { -1.0 });
        self.last_pos.set(pos);
        self.prev.replace(self.text.replace(value.to_string()));
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
            w.queue_draw();
            if me.t.get() >= 1.0 {
                me.tick.replace(None);
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
        self.tick.replace(Some(id));
    }

    // in @cells cells from @x0, the glyphs' tops at @top; @fallback while nothing has been set
    pub fn draw(&self, cr: &cairo::Context, fallback: &str, cells: usize, x0: f64, top: f64, pitch: f64, on: (f64, f64, f64, f64)) {
        let now = self.text.borrow().clone();
        let now = if now.is_empty() { fallback.to_string() } else { now };
        let (new, old) = (normalise(&now), normalise(&self.prev.borrow()));
        let t = self.t.get();
        let e = 1.0 - (1.0 - t).powi(3);
        let travel = 7.0 * pitch * 0.9;
        let dir = self.dir.get();
        let (nc, oc): (Vec<char>, Vec<char>) = (new.chars().collect(), old.chars().collect());
        cr.save().ok();
        cr.rectangle(x0 - 1.0, top - 1.0, cells_width(cells, pitch) + 2.0, 7.0 * pitch + 2.0);
        cr.clip();
        for i in 0..cells {
            let (n, o) = (nc.get(i).copied().unwrap_or(' '), oc.get(i).copied().unwrap_or(' '));
            let x = x0 + (6 * i) as f64 * pitch;
            if t >= 1.0 || n == o {
                draw_glyph(cr, n, x, top, pitch, on, 1.0);
            } else {
                draw_glyph(cr, o, x, top - dir * e * travel, pitch, on, 1.0 - e);
                draw_glyph(cr, n, x, top + dir * (1.0 - e) * travel, pitch, on, e);
            }
        }
        cr.restore().ok();
    }
}

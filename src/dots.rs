// Dot-matrix numerals, as on a piece of TE hardware: 5x7 glyphs drawn as round dots, the lit
// ones in a colour and the others faint. For a Canvas's draw function (a small texture,
// painted again only when the value changes).

use gtk::cairo;
use std::f64::consts::PI;

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

// the width @text takes with its dots @pitch apart (a glyph is 5 dots and a gap of one)
pub fn width(text: &str, pitch: f64) -> f64 {
    let n = normalise(text).chars().count() as f64;
    ((6.0 * n - 1.0).max(0.0) + 0.0) * pitch
}

// the pitch at which @text fits @room, at most @max
pub fn fit(text: &str, room: f64, max: f64) -> f64 {
    let n = normalise(text).chars().count() as f64;
    (room / (6.0 * n - 1.0).max(1.0)).min(max)
}

// @text centred on @cx, its top at @top; lit dots in @on, the rest faint white
pub fn draw(cr: &cairo::Context, text: &str, cx: f64, top: f64, pitch: f64, on: (f64, f64, f64, f64)) {
    let text = normalise(text);
    let x0 = cx - width(&text, pitch) / 2.0;
    let radius = pitch * 0.38;
    for (i, c) in text.chars().enumerate() {
        let rows = glyph(c);
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                let lit = bits >> (4 - col) & 1 == 1;
                let (x, y) = (x0 + ((6 * i + col) as f64 + 0.5) * pitch, top + (row as f64 + 0.5) * pitch);
                if lit {
                    cr.set_source_rgba(on.0, on.1, on.2, on.3);
                } else {
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.07);
                }
                cr.arc(x, y, radius, 0.0, 2.0 * PI);
                let _ = cr.fill();
            }
        }
    }
}

// Dot-matrix numerals, as on a piece of TE hardware: 5x7 glyphs drawn as round dots, the lit
// ones in a colour and the others faint, in a fixed number of cells, left-justified, the blank
// cells shown as unlit dots; a new value shows at once, as on real hardware. For a Canvas's draw
// function (a small texture, painted again only when the value changes).

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

// the width of @cells cells with the dots @pitch apart (a glyph is 5 dots and a gap of one)
pub fn cells_width(cells: usize, pitch: f64) -> f64 {
    (6.0 * cells as f64 - 1.0).max(0.0) * pitch
}

// @text in @cells cells, the first at @x0 (its left), the glyphs' tops at @top; a cell with no
// character is blank, drawn as unlit dots. The lit dots in @on.
pub fn draw_cells(cr: &cairo::Context, text: &str, cells: usize, x0: f64, top: f64, pitch: f64, on: (f64, f64, f64, f64)) {
    let text = normalise(text);
    let mut chars = text.chars();
    let radius = pitch * 0.4;
    for i in 0..cells {
        let c = chars.next().unwrap_or(' ');
        for (row, bits) in glyph(c).iter().enumerate() {
            for col in 0..5 {
                let lit = bits >> (4 - col) & 1 == 1;
                let (x, y) = (x0 + ((6 * i + col) as f64 + 0.5) * pitch, top + (row as f64 + 0.5) * pitch);
                if lit {
                    cr.set_source_rgba(on.0, on.1, on.2, on.3);
                } else {
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.09);
                }
                cr.arc(x, y, radius, 0.0, 2.0 * PI);
                let _ = cr.fill();
            }
        }
    }
}

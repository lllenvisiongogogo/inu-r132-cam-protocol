//! Drawing detection boxes and their labels into a minifb `0x00RRGGBB` buffer.
//!
//! Everything here is clipped: a box that leaves the frame, a label that runs off
//! the right edge or a coordinate that arrives as `NaN` must never panic the
//! window loop, it just draws less.

/// A colour in minifb's `0x00RRGGBB` layout.
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// Box colours, picked by class id so two models' classes stay distinguishable
/// at a glance. The label next to the box says which class it really is.
pub const PALETTE: [u32; 6] = [
    rgb(0, 255, 0),   // lime
    rgb(255, 170, 0), // orange
    rgb(0, 200, 255), // cyan
    rgb(255, 0, 255), // magenta
    rgb(255, 60, 60), // red
    rgb(255, 255, 0), // yellow
];

pub fn palette_colour(class_id: i32) -> u32 {
    let index = class_id.rem_euclid(PALETTE.len() as i32) as usize;
    PALETTE[index]
}

/// A 5x7 bitmap font, ASCII 0x20..=0x5F, five column bytes per glyph with bit 0
/// at the top. Uppercase only: labels are upper-cased before they are drawn.
const FONT: [[u8; 5]; 64] = [
    [0x00, 0x00, 0x00, 0x00, 0x00], // space
    [0x00, 0x00, 0x5f, 0x00, 0x00], // !
    [0x00, 0x07, 0x00, 0x07, 0x00], // "
    [0x14, 0x7f, 0x14, 0x7f, 0x14], // #
    [0x24, 0x2a, 0x7f, 0x2a, 0x12], // $
    [0x23, 0x13, 0x08, 0x64, 0x62], // %
    [0x36, 0x49, 0x55, 0x22, 0x50], // &
    [0x00, 0x05, 0x03, 0x00, 0x00], // '
    [0x00, 0x1c, 0x22, 0x41, 0x00], // (
    [0x00, 0x41, 0x22, 0x1c, 0x00], // )
    [0x14, 0x08, 0x3e, 0x08, 0x14], // *
    [0x08, 0x08, 0x3e, 0x08, 0x08], // +
    [0x00, 0x50, 0x30, 0x00, 0x00], // ,
    [0x08, 0x08, 0x08, 0x08, 0x08], // -
    [0x00, 0x60, 0x60, 0x00, 0x00], // .
    [0x20, 0x10, 0x08, 0x04, 0x02], // /
    [0x3e, 0x51, 0x49, 0x45, 0x3e], // 0
    [0x00, 0x42, 0x7f, 0x40, 0x00], // 1
    [0x42, 0x61, 0x51, 0x49, 0x46], // 2
    [0x21, 0x41, 0x45, 0x4b, 0x31], // 3
    [0x18, 0x14, 0x12, 0x7f, 0x10], // 4
    [0x27, 0x45, 0x45, 0x45, 0x39], // 5
    [0x3c, 0x4a, 0x49, 0x49, 0x30], // 6
    [0x01, 0x71, 0x09, 0x05, 0x03], // 7
    [0x36, 0x49, 0x49, 0x49, 0x36], // 8
    [0x06, 0x49, 0x49, 0x29, 0x1e], // 9
    [0x00, 0x36, 0x36, 0x00, 0x00], // :
    [0x00, 0x56, 0x36, 0x00, 0x00], // ;
    [0x00, 0x08, 0x14, 0x22, 0x41], // <
    [0x14, 0x14, 0x14, 0x14, 0x14], // =
    [0x41, 0x22, 0x14, 0x08, 0x00], // >
    [0x02, 0x01, 0x51, 0x09, 0x06], // ?
    [0x32, 0x49, 0x79, 0x41, 0x3e], // @
    [0x7e, 0x11, 0x11, 0x11, 0x7e], // A
    [0x7f, 0x49, 0x49, 0x49, 0x36], // B
    [0x3e, 0x41, 0x41, 0x41, 0x22], // C
    [0x7f, 0x41, 0x41, 0x22, 0x1c], // D
    [0x7f, 0x49, 0x49, 0x49, 0x41], // E
    [0x7f, 0x09, 0x09, 0x01, 0x01], // F
    [0x3e, 0x41, 0x41, 0x51, 0x32], // G
    [0x7f, 0x08, 0x08, 0x08, 0x7f], // H
    [0x00, 0x41, 0x7f, 0x41, 0x00], // I
    [0x20, 0x40, 0x41, 0x3f, 0x01], // J
    [0x7f, 0x08, 0x14, 0x22, 0x41], // K
    [0x7f, 0x40, 0x40, 0x40, 0x40], // L
    [0x7f, 0x02, 0x04, 0x02, 0x7f], // M
    [0x7f, 0x04, 0x08, 0x10, 0x7f], // N
    [0x3e, 0x41, 0x41, 0x41, 0x3e], // O
    [0x7f, 0x09, 0x09, 0x09, 0x06], // P
    [0x3e, 0x41, 0x51, 0x21, 0x5e], // Q
    [0x7f, 0x09, 0x19, 0x29, 0x46], // R
    [0x46, 0x49, 0x49, 0x49, 0x31], // S
    [0x01, 0x01, 0x7f, 0x01, 0x01], // T
    [0x3f, 0x40, 0x40, 0x40, 0x3f], // U
    [0x1f, 0x20, 0x40, 0x20, 0x1f], // V
    [0x7f, 0x20, 0x18, 0x20, 0x7f], // W
    [0x63, 0x14, 0x08, 0x14, 0x63], // X
    [0x03, 0x04, 0x78, 0x04, 0x03], // Y
    [0x61, 0x51, 0x49, 0x45, 0x43], // Z
    [0x00, 0x00, 0x7f, 0x41, 0x41], // [
    [0x02, 0x04, 0x08, 0x10, 0x20], // backslash
    [0x41, 0x41, 0x7f, 0x00, 0x00], // ]
    [0x04, 0x02, 0x01, 0x02, 0x04], // ^
    [0x40, 0x40, 0x40, 0x40, 0x40], // _
];

/// A picture to draw on: the minifb buffer plus its size.
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

impl Canvas<'_> {
    fn put(&mut self, x: i64, y: i64, colour: u32) {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return;
        }
        let index = y as usize * self.width + x as usize;
        if let Some(pixel) = self.pixels.get_mut(index) {
            *pixel = colour;
        }
    }

    /// The box in whole pixels, pulled in to a window around the canvas. A box
    /// far outside it is clamped rather than rejected: nothing inside the canvas
    /// can be within `thickness` of an edge that is further away than that, and
    /// the clamp is what stops a wild coordinate from spinning the loops below
    /// for hours. Saturating `f32 as i64` keeps `inf` and huge values bounded.
    fn box_bounds(
        &self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        thickness: usize,
    ) -> Option<(i64, i64, i64, i64, i64)> {
        if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
            return None;
        }
        let left = x.floor() as i64;
        let top = y.floor() as i64;
        let right = (x + w).ceil() as i64;
        let bottom = (y + h).ceil() as i64;
        if right <= left || bottom <= top {
            return None;
        }

        let t = thickness.max(1) as i64;
        let limit_x = self.width as i64 + t;
        let limit_y = self.height as i64 + t;
        Some((
            left.clamp(-t, limit_x),
            top.clamp(-t, limit_y),
            right.clamp(-t, limit_x),
            bottom.clamp(-t, limit_y),
            t,
        ))
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, colour: u32) {
        let Some((left, top, right, bottom, _)) = self.box_bounds(x, y, w, h, 1) else {
            return;
        };
        for py in top.max(0)..bottom.min(self.height as i64) {
            for px in left.max(0)..right.min(self.width as i64) {
                self.put(px, py, colour);
            }
        }
    }

    /// Outline of a box, `thickness` pixels wide inward.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, colour: u32, thickness: usize) {
        let Some((left, top, right, bottom, t)) = self.box_bounds(x, y, w, h, thickness) else {
            return;
        };
        for py in top.max(0)..bottom.min(self.height as i64) {
            for px in left.max(0)..right.min(self.width as i64) {
                let on_edge =
                    px - left < t || right - 1 - px < t || py - top < t || bottom - 1 - py < t;
                if on_edge {
                    self.put(px, py, colour);
                }
            }
        }
    }

    /// Size a `draw_text` will occupy, in pixels.
    pub fn text_size(&self, text: &str, scale: usize) -> (usize, usize) {
        let scale = scale.max(1);
        let glyphs = text.chars().count().max(1);
        (glyphs * 6 * scale, 7 * scale)
    }

    /// Draw uppercase text. Lowercase input is upper-cased; anything outside the
    /// font's range becomes a space.
    pub fn text(&mut self, x: f32, y: f32, text: &str, colour: u32, scale: usize) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let scale = scale.max(1) as i64;
        let ox = x.floor() as i64;
        let oy = y.floor() as i64;
        for (index, character) in text.chars().enumerate() {
            let byte = character.to_ascii_uppercase() as u32;
            let byte = if (0x20..=0x5f).contains(&byte) {
                byte
            } else {
                0x20
            };
            let glyph = FONT[(byte - 0x20) as usize];
            let gx = ox + index as i64 * 6 * scale;
            for (column, bits) in glyph.iter().enumerate() {
                for row in 0..7i64 {
                    if bits & (1 << row) == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            self.put(
                                gx + column as i64 * scale + dx,
                                oy + row * scale + dy,
                                colour,
                            );
                        }
                    }
                }
            }
        }
    }

    /// A small filled circle, marking where a distance was measured.
    pub fn dot(&mut self, x: f32, y: f32, radius: f32, colour: u32) {
        if !x.is_finite() || !y.is_finite() || !radius.is_finite() || radius <= 0.0 {
            return;
        }
        let centre_x = x.round() as i64;
        let centre_y = y.round() as i64;
        let reach = radius.ceil() as i64;
        let first_x = (centre_x - reach).max(0);
        let last_x = (centre_x + reach).min(self.width as i64 - 1);
        let first_y = (centre_y - reach).max(0);
        let last_y = (centre_y + reach).min(self.height as i64 - 1);
        for py in first_y..=last_y {
            for px in first_x..=last_x {
                let dx = (px - centre_x) as f32;
                let dy = (py - centre_y) as f32;
                if dx * dx + dy * dy <= radius * radius {
                    self.put(px, py, colour);
                }
            }
        }
    }

    /// Text with a dark plate behind it, so it stays readable over any picture.
    pub fn label(&mut self, x: f32, y: f32, text: &str, colour: u32, scale: usize) {
        let (w, h) = self.text_size(text, scale);
        self.fill_rect(
            x - 2.0,
            y - 1.0,
            w as f32 + 4.0,
            h as f32 + 2.0,
            rgb(0, 0, 0),
        );
        self.text(x, y, text, colour, scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(pixels: &mut [u32]) -> Canvas<'_> {
        Canvas {
            pixels,
            width: 32,
            height: 16,
        }
    }

    #[test]
    fn rect_draws_the_outline_only() {
        let mut pixels = vec![0u32; 32 * 16];
        canvas(&mut pixels).rect(4.0, 4.0, 6.0, 6.0, 0x00ff00, 1);
        assert_eq!(pixels[4 * 32 + 4], 0x00ff00, "corner is on the outline");
        assert_eq!(pixels[6 * 32 + 6], 0, "the middle stays untouched");
        assert_eq!(
            pixels[4 * 32 + 10],
            0,
            "just outside the box stays untouched"
        );
    }

    #[test]
    fn out_of_bounds_and_nonsense_are_clipped() {
        // This has to return quickly: iterating a box that is a billion pixels
        // wide would hang the render loop, so the loops are clipped first.
        let mut pixels = vec![0u32; 32 * 16];
        let mut canvas = canvas(&mut pixels);
        canvas.rect(-100.0, -100.0, 1e9, 1e9, 0xffffff, 3);
        canvas.rect(-1e30, -1e30, 1e30, 1e30, 0xffffff, 2);
        canvas.rect(0.0, 0.0, f32::INFINITY, f32::INFINITY, 0xffffff, 1);
        canvas.rect(f32::NAN, 0.0, 5.0, 5.0, 0xffffff, 1);
        canvas.rect(1000.0, 1000.0, 5.0, 5.0, 0xffffff, 1);
        canvas.rect(10.0, 8.0, -4.0, -4.0, 0xffffff, 1);
        canvas.fill_rect(-1e9, -1e9, 1e9, 1e9, 0xffffff);
        canvas.text(-5.0, -3.0, "hello", 0xffffff, 2);
        canvas.label(30.0, 15.0, "blue_cube 0.99", 0xffffff, 1);
    }

    #[test]
    fn a_box_bigger_than_the_canvas_still_draws_its_edges() {
        let mut pixels = vec![0u32; 32 * 16];
        // (-8,-8) 40x24 -> right edge at x=32 and bottom edge at y=16, both one
        // pixel past the canvas, so only those two edges are visible.
        canvas(&mut pixels).rect(-8.0, -8.0, 40.0, 24.0, 0x00ff00, 1);
        assert_eq!(pixels[15 * 32], 0x00ff00, "the bottom edge is drawn");
        assert_eq!(pixels[31], 0x00ff00, "the right edge is drawn");
        assert_eq!(pixels[0], 0, "the top left corner is not on an edge");
        assert_eq!(pixels[7 * 32 + 20], 0, "the interior stays clear");
    }

    #[test]
    fn text_lights_pixels_and_uppercases() {
        let mut lower = vec![0u32; 32 * 16];
        canvas(&mut lower).text(1.0, 1.0, "ab", 0x00ff00, 1);
        let mut upper = vec![0u32; 32 * 16];
        canvas(&mut upper).text(1.0, 1.0, "AB", 0x00ff00, 1);
        assert_eq!(lower, upper, "text is case insensitive");
        assert!(lower.contains(&0x00ff00), "something was drawn");
        assert_eq!(lower[0], 0, "the glyph does not start at the very corner");
    }

    #[test]
    fn class_colours_are_stable_and_wrap() {
        assert_eq!(palette_colour(0), PALETTE[0]);
        assert_eq!(palette_colour(1), PALETTE[1]);
        assert_eq!(palette_colour(6), PALETTE[0]);
        assert_eq!(palette_colour(-1), PALETTE[PALETTE.len() - 1]);
    }
}

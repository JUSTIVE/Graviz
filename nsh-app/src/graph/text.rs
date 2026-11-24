//! Text for the graph: glyphs as signed distance fields in one atlas.
//!
//! The graph's text is the same strings at every zoom, drawn anywhere from
//! three pixels tall to thirty. A bitmap atlas needs each glyph at each size,
//! which is what the gpui canvas did: a ladder of sizes, a cache miss on
//! every rung, lines dropped when a frame ran out of time, and text that was
//! the wrong width for its card between rungs. A distance field is one
//! bitmap per glyph that the fragment shader thresholds at whatever size it
//! lands, so a frame's text is a list of rectangles and costs the same
//! whether the zoom is moving or not.
//!
//! Small text is the exception. A field thresholded at eight pixels loses
//! the counters of an `e` and the joins of an `m`; below [`FIELD_FROM`]
//! device pixels a glyph is rasterized at exactly the size it is drawn,
//! into the same atlas. That is the ladder again, but without its costs:
//! the unit is a glyph, not a shaped line, a face has a hundred of them, and
//! rasterizing one takes microseconds, so a new size mid-zoom is paid for
//! inside the frame that needs it and nothing is ever left undrawn.

use std::collections::HashMap;

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _};

use crate::{model, theme::Color};

/// Pixels per em the fields are made at.
const FIELD_PX: f32 = 40.0;
/// How far, in field pixels, the distance reaches either side of the edge.
const SPREAD: f32 = 5.0;
/// The outline is rasterized this many times larger and thresholded, so the
/// distance is measured from a sharper edge than the field's own grid.
const SUPER: usize = 3;
pub const ATLAS: u32 = 2048;
/// Device-pixel size from which the distance field is used.
const FIELD_FROM: f32 = 22.0;
/// Bitmap sizes are rounded to this fraction of a device pixel.
const SIZE_STEP: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    /// Rows: they are code.
    Mono,
    MonoBold,
    MonoItalic,
    /// Captions and type names, the chrome's face.
    Sans,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphInstance {
    /// Screen points.
    pub rect: [f32; 4],
    /// Atlas texels.
    pub uv: [f32; 4],
    pub color: [f32; 4],
    /// x: 0 for a distance field, 1 for plain coverage.
    pub kind: [f32; 4],
}

#[derive(Clone, Copy)]
struct Glyph {
    /// Atlas cell, texels; zero-sized for a glyph with no ink.
    uv: [f32; 4],
    /// Cell's left and bottom relative to the pen and baseline, in ems.
    left: f32,
    bottom: f32,
    /// Cell size in ems.
    w: f32,
    h: f32,
    advance: f32,
}

pub struct Fonts {
    faces: HashMap<Face, FontVec>,
    /// What the faces lack (Hangul, the rest of CJK, everything else Arial
    /// Unicode covers), shared by every face and read only once a character
    /// needs them (with fontdue, which laid out every glyph of a font as it
    /// loaded it, these two held 2.4 GB while each face kept a copy).
    /// One slot per fallback file, filled the first time a character gets
    /// that far down the list (Hangul never needs Arial Unicode).
    fallbacks: Vec<(&'static str, Option<Option<FontVec>>)>,
    /// Keyed by size in quarter device pixels; 0 is the distance field.
    glyphs: HashMap<(Face, char, u16), Glyph>,
    /// Device pixels per point, set by the view before a frame is painted.
    pub scale: f32,
    pub pixels: Vec<u8>,
    /// Rows of the atlas written since the last upload.
    pub dirty: Option<(u32, u32)>,
    cursor: (u32, u32),
    shelf: u32,
}

/// A face read from `path` (`index` in a collection). ab_glyph reads each
/// outline when it is drawn, so a font costs its file and no more.
fn load(path: &str, index: u32) -> Option<FontVec> {
    let bytes = std::fs::read(path).ok()?;
    FontVec::try_from_vec_and_index(bytes, index).ok()
}

fn covers(font: &FontVec, c: char) -> bool {
    font.glyph_id(c).0 != 0
}

/// A rasterized glyph: its ink box in pixels, `xmin` / `ymin` from the pen
/// and the baseline (y up), and its advance.
struct Metrics {
    width: usize,
    height: usize,
    xmin: i32,
    ymin: i32,
    advance_width: f32,
}

/// `c` drawn `px` to the em: rows of coverage from the top.
fn rasterize(font: &FontVec, c: char, px: f32) -> (Metrics, Vec<u8>) {
    // ab_glyph's scale is the line height (ascent - descent); fontdue's,
    // which the layout was tuned on, is the em.
    let scale = PxScale::from(px * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0));
    let scaled = font.as_scaled(scale);
    let id = font.glyph_id(c);
    let advance_width = scaled.h_advance(id);
    let empty = (Metrics { width: 0, height: 0, xmin: 0, ymin: 0, advance_width }, Vec::new());
    let Some(outlined) = font.outline_glyph(id.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0))) else {
        return empty;
    };
    let b = outlined.px_bounds();
    let (width, height) = (b.width() as usize, b.height() as usize);
    if width == 0 || height == 0 {
        return empty;
    }
    let mut coverage = vec![0u8; width * height];
    outlined.draw(|x, y, v| {
        let i = y as usize * width + x as usize;
        if i < coverage.len() {
            coverage[i] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    });
    (Metrics { width, height, xmin: b.min.x as i32, ymin: -(b.max.y as i32), advance_width }, coverage)
}

impl Fonts {
    pub fn new() -> Self {
        let mut faces = HashMap::new();
        for (face, path, index) in [
            (Face::Mono, "/System/Library/Fonts/Menlo.ttc", 0),
            (Face::MonoBold, "/System/Library/Fonts/Menlo.ttc", 1),
            (Face::MonoItalic, "/System/Library/Fonts/Menlo.ttc", 2),
            (Face::Sans, "/System/Library/Fonts/HelveticaNeue.ttc", 0),
        ] {
            match load(path, index) {
                Some(font) => {
                    faces.insert(face, font);
                }
                None => eprintln!("graph: no font for {face:?} ({path})"),
            }
        }
        Self {
            faces,
            fallbacks: vec![("/System/Library/Fonts/AppleSDGothicNeo.ttc", None), ("/Library/Fonts/Arial Unicode.ttf", None)],
            glyphs: HashMap::new(),
            scale: 2.0,
            pixels: vec![0; (ATLAS * ATLAS) as usize],
            dirty: None,
            cursor: (1, 1),
            shelf: 0,
        }
    }

    /// The font that draws `c` in `face`: the face itself, else the first
    /// fallback that has it (loading them the first time one is needed),
    /// else the face's missing-glyph box.
    fn font_for(&mut self, face: Face, c: char) -> Option<&FontVec> {
        let has = self.faces.get(&face).is_some_and(|f| covers(f, c));
        let mut pick = None;
        if !has && !c.is_ascii() {
            for (i, (path, slot)) in self.fallbacks.iter_mut().enumerate() {
                let font = slot.get_or_insert_with(|| load(path, 0));
                if font.as_ref().is_some_and(|f| covers(f, c)) {
                    pick = Some(i);
                    break;
                }
            }
        }
        let this = &*self;
        let fallback = pick.and_then(|i| this.fallbacks[i].1.as_ref().and_then(Option::as_ref));
        fallback.or(this.faces.get(&face))
    }

    /// The glyph for `c` drawn `device_px` tall: `step` 0 is the field.
    fn glyph(&mut self, face: Face, c: char, step: u16) -> Glyph {
        if let Some(&g) = self.glyphs.get(&(face, c, step)) {
            return g;
        }
        let g = if step == 0 { self.make(face, c) } else { self.make_bitmap(face, c, step) };
        self.glyphs.insert((face, c, step), g);
        g
    }

    /// Room for a `w` by `h` cell, or `None` once the atlas has been started
    /// over to make it (every glyph made before is gone, and comes back the
    /// next time it is asked for).
    fn place(&mut self, w: u32, h: u32) -> (u32, u32) {
        if self.cursor.0 + w + 1 > ATLAS {
            self.cursor = (1, self.cursor.1 + self.shelf + 1);
            self.shelf = 0;
        }
        if self.cursor.1 + h + 1 > ATLAS {
            // Full: a long zoom has left a trail of sizes nobody is drawing
            // any more. Start over; what is on screen is remade this frame.
            self.glyphs.clear();
            self.pixels.fill(0);
            self.cursor = (1, 1);
            self.shelf = 0;
            self.dirty = Some((0, ATLAS));
        }
        let at = self.cursor;
        self.cursor.0 += w + 1;
        self.shelf = self.shelf.max(h);
        let (lo, hi) = self.dirty.unwrap_or((at.1, at.1));
        self.dirty = Some((lo.min(at.1), hi.max(at.1 + h)));
        at
    }

    fn make_bitmap(&mut self, face: Face, c: char, step: u16) -> Glyph {
        let empty = Glyph { uv: [0.0; 4], left: 0.0, bottom: 0.0, w: 0.0, h: 0.0, advance: 0.5 };
        let px = step as f32 / SIZE_STEP;
        let Some(font) = self.font_for(face, c) else { return empty };
        let (m, coverage) = rasterize(font, c, px);
        let advance = m.advance_width / px;
        if m.width == 0 || m.height == 0 {
            return Glyph { advance, ..empty };
        }
        // One clear texel all round, so sampling between pixels fades to
        // nothing rather than into a neighbour.
        let (w, h) = (m.width as u32 + 2, m.height as u32 + 2);
        let (ax, ay) = self.place(w, h);
        for y in 0..m.height {
            let row = ((ay + 1 + y as u32) * ATLAS + ax + 1) as usize;
            self.pixels[row..row + m.width].copy_from_slice(&coverage[y * m.width..(y + 1) * m.width]);
        }
        Glyph {
            uv: [ax as f32, ay as f32, w as f32, h as f32],
            left: (m.xmin as f32 - 1.0) / px,
            bottom: (m.ymin as f32 - 1.0) / px,
            w: w as f32 / px,
            h: h as f32 / px,
            advance,
        }
    }

    fn make(&mut self, face: Face, c: char) -> Glyph {
        let empty = Glyph { uv: [0.0; 4], left: 0.0, bottom: 0.0, w: 0.0, h: 0.0, advance: 0.5 };
        let Some(font) = self.font_for(face, c) else { return empty };
        let big = FIELD_PX * SUPER as f32;
        let (m, coverage) = rasterize(font, c, big);
        let advance = m.advance_width / big;
        if m.width == 0 || m.height == 0 {
            return Glyph { advance, ..empty };
        }
        // The field's cell: the ink box, grown by the spread, on the field's
        // own grid.
        let pad = SPREAD.ceil() as usize;
        let (fw, fh) = (m.width.div_ceil(SUPER) + 2 * pad, m.height.div_ceil(SUPER) + 2 * pad);
        let (bw, bh) = (fw * SUPER, fh * SUPER);
        let off = pad * SUPER;
        let mut inside = vec![false; bw * bh];
        for y in 0..m.height {
            for x in 0..m.width {
                inside[(y + off) * bw + x + off] = coverage[y * m.width + x] >= 128;
            }
        }
        let outer = distance(&inside, bw, bh, false);
        let inner = distance(&inside, bw, bh, true);
        let (ax, ay) = self.place(fw as u32, fh as u32);
        for y in 0..fh {
            for x in 0..fw {
                // The centre of this field texel on the big grid.
                let (bx, by) = (x * SUPER + SUPER / 2, y * SUPER + SUPER / 2);
                let i = by * bw + bx;
                let d = if inside[i] { inner[i].sqrt() - 0.5 } else { 0.5 - outer[i].sqrt() } / SUPER as f32;
                let v = (0.5 + d / (2.0 * SPREAD)).clamp(0.0, 1.0);
                self.pixels[((ay + y as u32) * ATLAS + ax + x as u32) as usize] = (v * 255.0).round() as u8;
            }
        }
        Glyph {
            uv: [ax as f32, ay as f32, fw as f32, fh as f32],
            left: (m.xmin as f32 / SUPER as f32 - pad as f32) / FIELD_PX,
            bottom: (m.ymin as f32 / SUPER as f32 - pad as f32) / FIELD_PX,
            w: fw as f32 / FIELD_PX,
            h: fh as f32 / FIELD_PX,
            advance,
        }
    }

    /// Appends `text` to `out` with its baseline at (`x`, `baseline`), `size`
    /// points tall, and returns how wide it came out.
    ///
    /// The mono faces are set on the model's grid, a fixed advance per cell,
    /// not on the font's own: the card widths and every right-aligned type
    /// name were measured on that grid, and text drawn on any other would
    /// not end where the layout says it does.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        out: &mut Vec<GlyphInstance>,
        face: Face,
        text: &str,
        size: f32,
        color: Color,
        x: f32,
        baseline: f32,
    ) -> f32 {
        let mut pen = x;
        let color = color.array();
        let device = size * self.scale;
        let step = if device >= FIELD_FROM { 0 } else { (device * SIZE_STEP).round().max(1.0) as u16 };
        // Draw at the size the glyphs were made for, so each texel lands on
        // one device pixel; the difference is under an eighth of a pixel.
        let (draw, kind) = if step == 0 { (size, 0.0) } else { (step as f32 / SIZE_STEP / self.scale, 1.0) };
        // Rows sit on whole device pixels, or their tops and bottoms blur.
        let baseline = if step == 0 { baseline } else { (baseline * self.scale).round() / self.scale };
        for c in text.chars() {
            let g = self.glyph(face, c, step);
            // Mono rows keep ASCII on the layout's grid; what comes from a
            // fallback (Hangul, CJK) takes its own advance, as CoreText sets
            // it in the gpui app. The layout's two cells for a wide
            // character are an upper bound, so the text still fits.
            let natural = face == Face::Sans || !c.is_ascii();
            let cell = if natural { g.advance * size } else { model::mono_w(c.encode_utf8(&mut [0; 4]), size) };
            if g.w > 0.0 {
                // Centre a glyph narrower or wider than its cell.
                let shift = if natural { 0.0 } else { (cell - g.advance * size) / 2.0 };
                out.push(GlyphInstance {
                    rect: [pen + shift + g.left * draw, baseline - (g.bottom + g.h) * draw, g.w * draw, g.h * draw],
                    uv: g.uv,
                    color,
                    kind: [kind, 0.0, 0.0, 0.0],
                });
            }
            pen += cell;
        }
        pen - x
    }

    /// Width of `text` in the sans face at `size`.
    pub fn sans_width(&mut self, text: &str, size: f32) -> f32 {
        text.chars().map(|c| self.glyph(Face::Sans, c, 0).advance * size).sum()
    }
}

/// Squared distance from every cell to the nearest cell that is (`to_inside`
/// false) inside or (true) outside the shape: the exact Euclidean transform,
/// one pass along rows and one along columns.
fn distance(inside: &[bool], w: usize, h: usize, to_outside: bool) -> Vec<f32> {
    const FAR: f32 = 1e12;
    let mut d: Vec<f32> = inside.iter().map(|&i| if i != to_outside { 0.0 } else { FAR }).collect();
    let n = w.max(h);
    let (mut f, mut out) = (vec![0.0f32; n], vec![0.0f32; n]);
    let (mut v, mut z) = (vec![0usize; n], vec![0.0f32; n + 1]);
    let mut pass = |f: &[f32], out: &mut [f32], len: usize| {
        let mut k = 0;
        v[0] = 0;
        z[0] = -FAR;
        z[1] = FAR;
        for q in 1..len {
            loop {
                let p = v[k];
                let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
                if s <= z[k] && k > 0 {
                    k -= 1;
                } else {
                    k += 1;
                    v[k] = q;
                    z[k] = s;
                    z[k + 1] = FAR;
                    break;
                }
            }
        }
        k = 0;
        for (q, o) in out.iter_mut().enumerate().take(len) {
            while z[k + 1] < q as f32 {
                k += 1;
            }
            let p = v[k];
            *o = (q as f32 - p as f32).powi(2) + f[p];
        }
    };
    for y in 0..h {
        f[..w].copy_from_slice(&d[y * w..(y + 1) * w]);
        pass(&f, &mut out, w);
        d[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
    }
    for x in 0..w {
        for y in 0..h {
            f[y] = d[y * w + x];
        }
        pass(&f, &mut out, h);
        for y in 0..h {
            d[y * w + x] = out[y];
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_euclidean() {
        // One inside cell in a 5x5 grid: every other cell's distance to it.
        let mut inside = vec![false; 25];
        inside[12] = true;
        let d = distance(&inside, 5, 5, false);
        assert_eq!(d[12], 0.0);
        assert_eq!(d[13], 1.0);
        assert_eq!(d[0], 8.0);
        assert_eq!(d[4 * 5 + 2], 4.0);
    }
}

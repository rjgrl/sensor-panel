//! Frame renderer — draws a theme + current sensor values into an RGBA buffer.
//! Geometry follows the old Pillow renderer (inclusive rectangle corners, arc
//! angles clockwise from 3 o'clock, text anchored at the ascender line) so
//! existing themes look the same on the screen.

use ab_glyph::{point, Font, FontArc, Glyph, PxScale, ScaleFont};
use chrono::{Datelike, Local};
use std::collections::{HashMap, VecDeque};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

use crate::fonts::Fonts;
use crate::theme::{BarStyle, Element, Kind, Rgba, TextMode, Theme};

pub struct Renderer {
    pub fonts: Fonts,
    history: HashMap<String, (usize, VecDeque<f64>)>,
    frame: Option<Pixmap>,
}

fn paint(c: Rgba, aa: bool) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], c[3]);
    p.anti_alias = aa;
    p
}

/// PIL-style inclusive rectangle [x0, y0, x1, y1]
fn fill_rect(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgba) {
    if c[3] == 0 {
        return;
    }
    if let Some(r) = Rect::from_ltrb(x0, y0, x1 + 1.0, y1 + 1.0) {
        pm.fill_rect(r, &paint(c, false), Transform::identity(), None);
    }
}

fn rounded_rect_path(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Option<Path> {
    let (l, t, rr, b) = (x0, y0, x1 + 1.0, y1 + 1.0);
    let w = rr - l;
    let h = b - t;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    if r <= 0.0 {
        return Some(PathBuilder::from_rect(Rect::from_ltrb(l, t, rr, b)?));
    }
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(l + r, t);
    pb.line_to(rr - r, t);
    pb.cubic_to(rr - r + k, t, rr, t + r - k, rr, t + r);
    pb.line_to(rr, b - r);
    pb.cubic_to(rr, b - r + k, rr - r + k, b, rr - r, b);
    pb.line_to(l + r, b);
    pb.cubic_to(l + r - k, b, l, b - r + k, l, b - r);
    pb.line_to(l, t + r);
    pb.cubic_to(l, t + r - k, l + r - k, t, l + r, t);
    pb.close();
    pb.finish()
}

fn fill_rounded(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, c: Rgba) {
    if c[3] == 0 {
        return;
    }
    if r <= 0.0 {
        fill_rect(pm, x0, y0, x1, y1, c);
        return;
    }
    if let Some(p) = rounded_rect_path(x0, y0, x1, y1, r) {
        pm.fill_path(&p, &paint(c, true), FillRule::Winding, Transform::identity(), None);
    }
}

/// PIL rectangle(outline=, width=t): t-pixel border drawn inward
fn outline_rect(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, t: f32, c: Rgba) {
    if t <= 0.0 || c[3] == 0 {
        return;
    }
    fill_rect(pm, x0, y0, x1, y0 + t - 1.0, c);
    fill_rect(pm, x0, y1 - t + 1.0, x1, y1, c);
    fill_rect(pm, x0, y0 + t, x0 + t - 1.0, y1 - t, c);
    fill_rect(pm, x1 - t + 1.0, y0 + t, x1, y1 - t, c);
}

/// PIL arc(box, start, end, width): clockwise from 3 o'clock, width drawn inward
fn arc(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, start: f32, end: f32, width: f32, c: Rgba) {
    let sweep = end - start;
    if sweep <= 0.0 || width <= 0.0 || c[3] == 0 {
        return;
    }
    let cx = (x0 + x1 + 1.0) / 2.0;
    let cy = (y0 + y1 + 1.0) / 2.0;
    let rx = (x1 - x0 + 1.0) / 2.0;
    let r = rx - width / 2.0;
    if r <= 0.0 {
        return;
    }
    let path = if sweep >= 360.0 {
        PathBuilder::from_circle(cx, cy, r)
    } else {
        let mut pb = PathBuilder::new();
        let n = (sweep / 90.0).ceil().max(1.0) as usize;
        let step = sweep.to_radians() / n as f32;
        let mut a = start.to_radians();
        pb.move_to(cx + r * a.cos(), cy + r * a.sin());
        for _ in 0..n {
            let b = a + step;
            let k = 4.0 / 3.0 * ((b - a) / 4.0).tan();
            let (ca, sa, cb, sb) = (a.cos(), a.sin(), b.cos(), b.sin());
            pb.cubic_to(
                cx + r * (ca - k * sa),
                cy + r * (sa + k * ca),
                cx + r * (cb + k * sb),
                cy + r * (sb - k * cb),
                cx + r * cb,
                cy + r * sb,
            );
            a = b;
        }
        pb.finish()
    };
    if let Some(p) = path {
        let stroke = Stroke { width, line_cap: LineCap::Butt, ..Stroke::default() };
        pm.stroke_path(&p, &paint(c, true), &stroke, Transform::identity(), None);
    }
}

// ── text ────────────────────────────────────────────────────────────────────

struct Laid {
    glyphs: Vec<Glyph>,
    /// advance width (like PIL textbbox width)
    width: f32,
    /// ink top / bottom relative to the draw origin (the ascender line)
    ink_top: f32,
    ink_bottom: f32,
}

fn px_scale(font: &FontArc, size: f32) -> PxScale {
    let upem = font.units_per_em().unwrap_or(1000.0);
    let h = font.height_unscaled();
    PxScale::from(if upem > 0.0 && h > 0.0 { size * h / upem } else { size })
}

fn layout(font: &FontArc, size: f32, text: &str) -> Laid {
    let scale = px_scale(font, size);
    let sf = font.as_scaled(scale);
    let ascent = sf.ascent();
    let mut caret = 0.0f32;
    let mut prev = None;
    let mut glyphs = Vec::with_capacity(text.len());
    let mut ink_top = f32::MAX;
    let mut ink_bottom = f32::MIN;
    for c in text.chars() {
        if c.is_control() {
            continue;
        }
        let id = sf.glyph_id(c);
        if let Some(p) = prev {
            caret += sf.kern(p, id);
        }
        let g = id.with_scale_and_position(scale, point(caret, ascent));
        if let Some(og) = font.outline_glyph(g.clone()) {
            let b = og.px_bounds();
            ink_top = ink_top.min(b.min.y);
            ink_bottom = ink_bottom.max(b.max.y);
        }
        caret += sf.h_advance(id);
        prev = Some(id);
        glyphs.push(g);
    }
    if ink_top == f32::MAX {
        ink_top = 0.0;
        ink_bottom = 0.0;
    }
    Laid { glyphs, width: caret, ink_top, ink_bottom }
}

fn draw_glyphs(pm: &mut Pixmap, font: &FontArc, laid: &Laid, ox: f32, oy: f32, c: Rgba) {
    let w = pm.width() as i32;
    let h = pm.height() as i32;
    let data = pm.data_mut();
    let ca = c[3] as f32 / 255.0;
    let src = [c[0] as f32 * ca, c[1] as f32 * ca, c[2] as f32 * ca, c[3] as f32];
    for g in &laid.glyphs {
        let mut g = g.clone();
        g.position = point(g.position.x + ox, g.position.y + oy);
        if let Some(og) = font.outline_glyph(g) {
            let b = og.px_bounds();
            let bx = b.min.x as i32;
            let by = b.min.y as i32;
            og.draw(|gx, gy, cov| {
                let px = bx + gx as i32;
                let py = by + gy as i32;
                if px < 0 || py < 0 || px >= w || py >= h {
                    return;
                }
                let a = cov.clamp(0.0, 1.0) * ca;
                if a <= 0.0 {
                    return;
                }
                let cov = cov.clamp(0.0, 1.0);
                let i = ((py * w + px) * 4) as usize;
                for k in 0..4 {
                    let d = data[i + k] as f32;
                    data[i + k] = (src[k] * cov + d * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
                }
            });
        }
    }
}

/// Python '//' (floor division)
fn fdiv(a: i32, b: i32) -> i32 {
    a.div_euclid(b)
}

fn fmt_sensor(v: Option<f64>, unit: &str) -> String {
    match v {
        None => "0".into(),
        Some(raw) if !raw.is_finite() => "0".into(),
        Some(raw) => {
            const WHOLE: [&str; 6] = ["%", "RPM", "\u{00B0}C", "C", "MHz", "W"];
            if WHOLE.contains(&unit.trim()) || raw == raw.trunc() {
                format!("{}", raw.round_ties_even() as i64)
            } else {
                format!("{raw:.1}")
            }
        }
    }
}

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];

fn fmt_date(fmt: &str, now: &chrono::DateTime<Local>) -> String {
    let month = MONTHS[now.month0() as usize];
    let mut out = String::new();
    let mut rest = fmt;
    while !rest.is_empty() {
        let (tok, rep): (&str, String) = if rest.starts_with("YYYY") {
            ("YYYY", format!("{:04}", now.year()))
        } else if rest.starts_with("YY") {
            ("YY", format!("{:02}", now.year().rem_euclid(100)))
        } else if rest.starts_with("MMMM") {
            ("MMMM", month.to_string())
        } else if rest.starts_with("MMM") {
            ("MMM", month[..3].to_string())
        } else if rest.starts_with("MM") {
            ("MM", format!("{:02}", now.month()))
        } else if rest.starts_with("DD") {
            ("DD", format!("{:02}", now.day()))
        } else if rest.starts_with('D') {
            ("D", now.day().to_string())
        } else {
            let c = rest.chars().next().unwrap();
            (&rest[..c.len_utf8()], c.to_string())
        };
        out.push_str(&rep);
        rest = &rest[tok.len()..];
    }
    out
}

impl Renderer {
    pub fn new() -> Self {
        Renderer { fonts: Fonts::default(), history: HashMap::new(), frame: None }
    }

    /// Called when a new theme is loaded
    pub fn reset(&mut self, custom_fonts: &[(String, FontArc)]) {
        self.history.clear();
        self.fonts.set_custom(custom_fonts);
        self.frame = None;
    }

    pub fn render(&mut self, theme: &Theme, sensors: &HashMap<String, f64>, fps: u32) -> &Pixmap {
        let need_new = match &self.frame {
            Some(f) => f.width() != theme.width || f.height() != theme.height,
            None => true,
        };
        if need_new {
            self.frame = Pixmap::new(theme.width, theme.height);
        }
        let mut frame = self.frame.take().expect("theme size validated on load");
        frame.data_mut().copy_from_slice(theme.base.data());

        let now = Local::now();
        for el in &theme.elements {
            self.draw_element(&mut frame, el, sensors, fps, &now);
        }
        self.frame = Some(frame);
        self.frame.as_ref().unwrap()
    }

    fn draw_element(
        &mut self,
        pm: &mut Pixmap,
        el: &Element,
        sensors: &HashMap<String, f64>,
        fps: u32,
        now: &chrono::DateTime<Local>,
    ) {
        let (x, y, w, h) = (el.x, el.y, el.w, el.h);
        let (xf, yf, wf, hf) = (x as f32, y as f32, w as f32, h as f32);
        let sv = |k: &str| sensors.get(k).copied();
        match &el.kind {
            Kind::Text(t) => {
                let text = match &t.mode {
                    TextMode::Clock { h12, seconds } => match (h12, seconds) {
                        // With seconds: zero-padded hour so AM/PM stays put
                        (true, true) => now.format("%I:%M:%S %p").to_string(),
                        (true, false) => now.format("%I:%M %p").to_string().trim_start_matches('0').to_string(),
                        (false, true) => now.format("%H:%M:%S").to_string(),
                        (false, false) => now.format("%H:%M").to_string(),
                    },
                    TextMode::Date(f) => fmt_date(f, now),
                    TextMode::Weekday { full } => now.format(if *full { "%A" } else { "%a" }).to_string(),
                    TextMode::Static(s) => s.clone(),
                    TextMode::Sensor(key) => {
                        let v = fmt_sensor(sv(key), &t.unit);
                        let sep = if !t.unit.is_empty() && !v.is_empty() { " " } else { "" };
                        format!("{}{}{}{}", t.prefix, v, sep, t.unit)
                    }
                };
                let font = match self.fonts.get(&t.family, t.bold) {
                    Some(f) => f,
                    None => return,
                };
                let laid = layout(&font, t.font_size, &text);
                let tw = laid.width.round() as i32;
                let tx = match t.align.as_str() {
                    "center" => x + fdiv(w - tw, 2),
                    "right" => x + w - tw,
                    _ => x,
                };
                draw_glyphs(pm, &font, &laid, tx as f32, yf, t.color);
            }

            Kind::Bar(b) => {
                let raw = sv(&b.key).unwrap_or(0.0);
                let pct = if b.max != 0.0 { (raw / b.max).clamp(0.0, 1.0) } else { 0.0 };
                let pct = if pct.is_nan() { 0.0 } else { pct };
                let rad = b.radius as f32;
                let (x1, y1) = (xf + wf, yf + hf);
                match b.style {
                    BarStyle::Segmented { segments, gap_pct } => {
                        // Discrete LED-style blocks — each fully lit or unlit
                        let lit = (pct * segments as f64).round_ties_even() as i32;
                        let full = wf / segments as f32;
                        let sw = full * (1.0 - gap_pct);
                        let srad = rad.min(3.0);
                        for i in 0..segments {
                            let sx = xf + i as f32 * full;
                            let c = if i < lit { b.fill } else { b.bg };
                            fill_rounded(pm, sx, yf, sx + sw, y1, srad, c);
                        }
                    }
                    BarStyle::Gapped { segments, gap_px } => {
                        fill_rounded(pm, xf, yf, x1, y1, rad, b.bg);
                        let fw = (wf as f64 * pct) as i32;
                        if fw > 0 {
                            fill_rounded(pm, xf, yf, xf + fw as f32, y1, rad, b.fill);
                        }
                        // gap lines in the background colour, cutting through fill and empty zones
                        let full = wf / segments as f32;
                        let g = gap_px as f32 / 2.0;
                        for i in 1..segments {
                            let gx = xf + i as f32 * full;
                            fill_rect(pm, gx - g, yf, gx + g, y1, b.bg);
                        }
                        outline_rect(pm, xf, yf, x1, y1, b.border as f32, b.border_color);
                    }
                    BarStyle::Solid => {
                        fill_rounded(pm, xf, yf, x1, y1, rad, b.bg);
                        let fw = (wf as f64 * pct) as i32;
                        if fw > 0 {
                            fill_rounded(pm, xf, yf, xf + fw as f32, y1, rad, b.fill);
                        }
                        outline_rect(pm, xf, yf, x1, y1, b.border as f32, b.border_color);
                    }
                }
            }

            Kind::Ring(r) => {
                let raw = sv(&r.key).unwrap_or(0.0);
                let pct = if r.max != 0.0 { (raw / r.max).clamp(0.0, 1.0) } else { 0.0 };
                let pct = if pct.is_nan() { 0.0 } else { pct };
                let rw = r.ring_width;
                let diam = w.min(h);
                let m = fdiv(rw, 2) + 2;
                let (bx0, by0, bx1, by1) = ((x + m) as f32, (y + m) as f32, (x + diam - m) as f32, (y + diam - m) as f32);
                match r.segmented {
                    Some((segs, gap)) => {
                        let seg_deg = 360.0 / segs as f32;
                        let arc_deg = (seg_deg - gap).max(0.5);
                        let lit = (pct * segs as f64).round_ties_even() as i32;
                        for i in 0..segs {
                            let s = -90.0 + i as f32 * seg_deg + gap / 2.0;
                            let c = if i < lit { r.arc } else { r.track };
                            arc(pm, bx0, by0, bx1, by1, s, s + arc_deg, rw as f32, c);
                        }
                    }
                    None => {
                        arc(pm, bx0, by0, bx1, by1, 0.0, 360.0, rw as f32, r.track);
                        if pct > 0.0 {
                            let end = -90.0 + (360.0 * pct).trunc() as f32;
                            arc(pm, bx0, by0, bx1, by1, -90.0, end, rw as f32, r.arc);
                        }
                    }
                }
                if r.show_label {
                    let label = format!("{}{}", if raw.is_finite() { raw.trunc() as i64 } else { 0 }, r.unit);
                    if let Some(font) = self.fonts.get(&r.label_family, r.label_bold) {
                        let laid = layout(&font, r.label_size, &label);
                        let tw = laid.width.round() as i32;
                        let th = (laid.ink_bottom - laid.ink_top).round() as i32;
                        let cx = x + fdiv(diam, 2) - fdiv(tw, 2);
                        let cy = y + fdiv(diam, 2) - fdiv(th, 2);
                        draw_glyphs(pm, &font, &laid, cx as f32, cy as f32, r.label_color);
                    }
                }
            }

            Kind::Graph(g) => {
                let maxlen = (g.history_seconds.max(0) as usize * fps.max(1) as usize).max(10);
                fill_rounded(pm, xf, yf, xf + wf, yf + hf, g.radius as f32, g.bg);
                if g.show_grid {
                    for gi in 1..4 {
                        let gy = y + fdiv(h * gi, 4);
                        fill_rect(pm, xf, gy as f32, xf + wf, gy as f32, g.grid);
                    }
                }
                for (si, s) in g.series.iter().enumerate() {
                    if s.key.is_empty() {
                        continue;
                    }
                    let raw = sv(&s.key).unwrap_or(0.0);
                    let hk = format!("{}_{}", el.id, si);
                    let entry = self.history.entry(hk).or_insert_with(|| (maxlen, VecDeque::with_capacity(maxlen.min(4096))));
                    let (cap, buf) = entry;
                    buf.push_back(raw);
                    while buf.len() > *cap {
                        buf.pop_front();
                    }
                    let n = buf.len();
                    if n < 2 {
                        continue;
                    }
                    let smax = s.max;
                    let coords: Vec<(f32, f32)> = buf
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let gx = x + (i as f64 / (n - 1) as f64 * w as f64) as i32;
                            let f = if smax != 0.0 { (v / smax).clamp(0.0, 1.0) } else { 0.0 };
                            let f = if f.is_nan() { 0.0 } else { f };
                            let gy = y + h - (f * h as f64 * 0.88 + h as f64 * 0.06) as i32;
                            (gx as f32, gy as f32)
                        })
                        .collect();
                    // Only series 1 gets a fill-under (matches the builder preview)
                    if si == 0 {
                        if let Some(fc) = s.fill {
                            let mut pb = PathBuilder::new();
                            pb.move_to(xf, yf + hf);
                            for (px, py) in &coords {
                                pb.line_to(*px, *py);
                            }
                            pb.line_to(xf + wf, yf + hf);
                            pb.close();
                            if let Some(p) = pb.finish() {
                                pm.fill_path(&p, &paint(fc, true), FillRule::Winding, Transform::identity(), None);
                            }
                        }
                    }
                    let mut pb = PathBuilder::new();
                    // pixel-centre the polyline like PIL's line drawing
                    pb.move_to(coords[0].0 + 0.5, coords[0].1 + 0.5);
                    for (px, py) in &coords[1..] {
                        pb.line_to(px + 0.5, py + 0.5);
                    }
                    if let Some(p) = pb.finish() {
                        let stroke = Stroke {
                            width: g.line_width.max(1.0),
                            line_cap: LineCap::Butt,
                            line_join: LineJoin::Round,
                            ..Stroke::default()
                        };
                        pm.stroke_path(&p, &paint(s.color, true), &stroke, Transform::identity(), None);
                    }
                }
            }

            Kind::Rect { fill, radius } => {
                fill_rounded(pm, xf, yf, xf + wf, yf + hf, *radius as f32, *fill);
            }

            Kind::Image(img) => {
                pm.draw_pixmap(x, y, img.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sensor_format() {
        assert_eq!(fmt_sensor(Some(45.25), "°C"), "45");
        assert_eq!(fmt_sensor(Some(1.25), "V"), "1.2");
        assert_eq!(fmt_sensor(Some(3.0), "GB"), "3");
        assert_eq!(fmt_sensor(None, "%"), "0");
    }

    #[test]
    fn date_format() {
        let d = Local.with_ymd_and_hms(2026, 12, 5, 10, 0, 0).unwrap();
        assert_eq!(fmt_date("DD-MM-YYYY", &d), "05-12-2026");
        assert_eq!(fmt_date("MMMM D, YYYY", &d), "December 5, 2026");
        assert_eq!(fmt_date("DD MMM YY", &d), "05 Dec 26");
    }
}

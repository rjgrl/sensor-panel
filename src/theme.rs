//! Theme loading (.ds916theme JSON with embedded data URLs, or a .zip export).
//!
//! Everything a frame needs is decoded once here — images are decoded and
//! pre-resized to their element size, fonts are parsed, colours are parsed —
//! and the raw JSON (which can be many MB of base64) is dropped right away, so
//! the per-frame renderer does no parsing and the background process stays small.

use base64::Engine;
use serde_json::{Map, Value};
use std::io::Read;
use std::path::Path;
use tiny_skia::{IntSize, Pixmap};

pub type Rgba = [u8; 4];

pub struct Theme {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Background colour with the background image (if any) already composited
    /// on top, pre-resized to width x height.
    pub base: Pixmap,
    pub elements: Vec<Element>,
    /// CUSTOM_* sensor keys -> LHM SensorId
    pub custom_map: Vec<(String, String)>,
    /// fonts embedded in the theme (family, parsed font); handed to the font cache
    pub custom_fonts: Vec<(String, ab_glyph::FontArc)>,
}

pub struct Element {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub kind: Kind,
}

pub enum TextMode {
    Sensor(String),
    Static(String),
    Clock { h12: bool, seconds: bool },
    Date(String),
    Weekday { full: bool },
}

pub struct TextEl {
    pub mode: TextMode,
    pub font_size: f32,
    pub family: String,
    pub bold: bool,
    pub color: Rgba,
    pub align: String,
    pub prefix: String,
    pub unit: String,
}

pub enum BarStyle {
    Solid,
    Segmented { segments: i32, gap_pct: f32 },
    Gapped { segments: i32, gap_px: i32 },
}

pub struct BarEl {
    pub key: String,
    pub max: f64,
    pub radius: i32,
    pub border: i32,
    pub bg: Rgba,
    pub fill: Rgba,
    pub border_color: Rgba,
    pub style: BarStyle,
}

pub struct RingEl {
    pub key: String,
    pub max: f64,
    pub ring_width: i32,
    pub arc: Rgba,
    pub track: Rgba,
    /// Some((segments, gap degrees)) for the segmented style
    pub segmented: Option<(i32, f32)>,
    pub show_label: bool,
    pub label_size: f32,
    pub label_family: String,
    pub label_bold: bool,
    pub label_color: Rgba,
    pub unit: String,
}

pub struct Series {
    pub key: String,
    pub color: Rgba,
    pub max: f64,
    pub fill: Option<Rgba>,
}

pub struct GraphEl {
    pub history_seconds: i32,
    pub series: Vec<Series>,
    pub line_width: f32,
    pub radius: i32,
    pub bg: Rgba,
    pub grid: Rgba,
    pub show_grid: bool,
}

pub enum Kind {
    Text(TextEl),
    Bar(BarEl),
    Ring(RingEl),
    Graph(GraphEl),
    Rect { fill: Rgba, radius: i32 },
    Image(Pixmap),
}

// ── value helpers (lenient, like the Python version's int()/float()) ────────

fn num(o: &Map<String, Value>, k: &str, d: f64) -> f64 {
    match o.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(d),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(d),
        Some(Value::Bool(b)) => {
            if *b { 1.0 } else { 0.0 }
        }
        _ => d,
    }
}

fn int(o: &Map<String, Value>, k: &str, d: i32) -> i32 {
    let v = num(o, k, d as f64);
    if v.is_finite() { v.trunc().clamp(-1_000_000.0, 1_000_000.0) as i32 } else { d }
}

/// Like `el.get(k) or default` for counts where 0 makes no sense
fn int_nonzero(o: &Map<String, Value>, k: &str, d: i32) -> i32 {
    let v = int(o, k, d);
    if v <= 0 { d } else { v }
}

fn string(o: &Map<String, Value>, k: &str, d: &str) -> String {
    match o.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => d.to_string(),
    }
}

/// Python truthiness of el.get(k, default)
fn truthy(o: &Map<String, Value>, k: &str, d: bool) -> bool {
    match o.get(k) {
        None => d,
        Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|v| v != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// '#rrggbbaa' (builder format) or '#rrggbb' -> rgba. Invalid -> white.
pub fn parse_color(c: &str) -> Rgba {
    const WHITE: Rgba = [255, 255, 255, 255];
    if c.len() < 7 {
        return WHITE;
    }
    let h = c.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => match (p(0), p(2), p(4)) {
            (Some(r), Some(g), Some(b)) => [r, g, b, 255],
            _ => WHITE,
        },
        8 => match (p(0), p(2), p(4), p(6)) {
            (Some(r), Some(g), Some(b), Some(a)) => [r, g, b, a],
            _ => WHITE,
        },
        _ => WHITE,
    }
}

fn color(o: &Map<String, Value>, k: &str, d: &str) -> Rgba {
    match o.get(k) {
        Some(Value::String(s)) => parse_color(s),
        Some(Value::Null) => parse_color(""),
        _ => parse_color(d),
    }
}

/// colour with alpha dropped (the Python renderer used color_rgb for these)
fn color_opaque(o: &Map<String, Value>, k: &str, d: &str) -> Rgba {
    let mut c = color(o, k, d);
    c[3] = 255;
    c
}

// ── data URLs / images ──────────────────────────────────────────────────────

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
}

/// "data:image/png;base64,...." -> (header, bytes)
pub fn decode_data_url(s: &str) -> Option<(String, Vec<u8>)> {
    let (header, data) = s.split_once(',')?;
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = b64().decode(cleaned.as_bytes()).ok()?;
    Some((header.to_string(), bytes))
}

/// Straight RGBA -> premultiplied tiny-skia Pixmap
fn rgba_to_pixmap(img: image::RgbaImage) -> Option<Pixmap> {
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a < 255 {
            px[0] = ((px[0] as u16 * a + 127) / 255) as u8;
            px[1] = ((px[1] as u16 * a + 127) / 255) as u8;
            px[2] = ((px[2] as u16 * a + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(data, IntSize::from_wh(w, h)?)
}

/// Decode image bytes and resize to exactly w x h (Lanczos, like PIL LANCZOS).
pub fn decode_resized(bytes: &[u8], w: i32, h: i32) -> Result<Pixmap, String> {
    if w <= 0 || h <= 0 {
        return Err(format!("invalid size {w}x{h}"));
    }
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8();
    let img = if img.width() == w as u32 && img.height() == h as u32 {
        img
    } else {
        image::imageops::resize(&img, w as u32, h as u32, image::imageops::FilterType::Lanczos3)
    };
    rgba_to_pixmap(img).ok_or_else(|| "image conversion failed".to_string())
}

// ── loading ─────────────────────────────────────────────────────────────────

struct Source {
    json: Value,
    zip: Option<zip::ZipArchive<std::fs::File>>,
}

fn read_zip_entry(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<Vec<u8>> {
    let mut f = z.by_name(name).ok()?;
    let mut v = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

fn open_source(path: &Path) -> Result<Source, String> {
    let lower = path.to_string_lossy().to_lowercase();
    if lower.ends_with(".zip") {
        let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        let tfile = z
            .file_names()
            .find(|n| n.to_lowercase().ends_with(".ds916theme"))
            .map(|s| s.to_string())
            .ok_or("no .ds916theme file inside the zip")?;
        let data = read_zip_entry(&mut z, &tfile).ok_or("could not read theme from zip")?;
        let text = String::from_utf8_lossy(&data);
        let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        Ok(Source { json, zip: Some(z) })
    } else if lower.ends_with(".ds916theme") {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        Ok(Source { json, zip: None })
    } else {
        Err("not a .ds916theme or .zip file".into())
    }
}

/// Only the name and resolution (for the Status window), without decoding assets.
pub fn peek_info(path: &Path) -> Option<(String, u32, u32)> {
    let src = open_source(path).ok()?;
    let o = src.json.as_object()?;
    Some((
        string(o, "name", "?"),
        int(o, "width", 462).max(1) as u32,
        int(o, "height", 1920).max(1) as u32,
    ))
}

pub fn load(path: &Path) -> Result<Theme, String> {
    let Source { json, mut zip } = open_source(path)?;
    let o = json.as_object().ok_or("theme file is not a JSON object")?;

    let width = int(o, "width", 462).clamp(1, 8192) as u32;
    let height = int(o, "height", 1920).clamp(1, 8192) as u32;
    let bg = color_opaque(o, "background", "#111114ff");

    // Base layer: background colour + background image
    let mut base = Pixmap::new(width, height).ok_or("invalid theme size")?;
    base.fill(tiny_skia::Color::from_rgba8(bg[0], bg[1], bg[2], 255));
    let mut bg_bytes: Option<Vec<u8>> = None;
    if let Some(z) = zip.as_mut() {
        let back = z.file_names().find(|n| n.to_lowercase().starts_with("back.")).map(|s| s.to_string());
        if let Some(b) = back {
            bg_bytes = read_zip_entry(z, &b);
        }
    }
    if bg_bytes.is_none() {
        if let Some(Value::String(du)) = o.get("backgroundImage") {
            if du.contains(',') {
                let header = du.split(',').next().unwrap_or("").to_lowercase();
                if header.contains("svg") {
                    crate::log_warn!(
                        "Background image is SVG format, which can't be decoded - this theme was likely generated by \
                         an older version of the AI Theme Generator. Re-generate and re-save the theme in the theme \
                         builder to fix (newer versions export a PNG background instead)."
                    );
                } else if header.contains("video") {
                    crate::log_warn!("Background is a video, which the screen renderer can't play - using the background colour");
                } else {
                    match decode_data_url(du) {
                        Some((_, b)) => bg_bytes = Some(b),
                        None => crate::log_warn!("Background image decode error: invalid base64 data"),
                    }
                }
            }
        }
    }
    if let Some(b) = bg_bytes {
        match decode_resized(&b, width as i32, height as i32) {
            Ok(p) => {
                base.draw_pixmap(0, 0, p.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
                crate::log_info!("Background image loaded ({}KB)", b.len() / 1024);
            }
            Err(e) => crate::log_warn!("Background image decode error: {e}"),
        }
    }

    // Elements, sorted by z ascending (lower z = further back). Stable sort.
    let mut raw: Vec<&Map<String, Value>> = o
        .get("elements")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|e| e.as_object()).collect())
        .unwrap_or_default();
    raw.sort_by(|a, b| num(a, "z", 0.0).partial_cmp(&num(b, "z", 0.0)).unwrap_or(std::cmp::Ordering::Equal));

    let mut elements = Vec::with_capacity(raw.len());
    for el in raw {
        if !truthy(el, "visible", true) {
            continue;
        }
        let typ = string(el, "type", "");
        let x = int(el, "x", 0);
        let y = int(el, "y", 0);
        let w = int(el, "w", 100);
        let h = int(el, "h", 30);
        let kind = match typ.as_str() {
            "text" | "static" | "clock" | "date" | "weekday" => {
                let mode = match typ.as_str() {
                    "clock" => TextMode::Clock {
                        h12: string(el, "clockFormat", "12h") == "12h",
                        seconds: truthy(el, "clockSeconds", true),
                    },
                    "date" => TextMode::Date(string(el, "dateFormat", "DD-MM-YYYY")),
                    "weekday" => TextMode::Weekday { full: string(el, "weekdayFormat", "full") == "full" },
                    "static" => TextMode::Static(string(el, "customText", "Label")),
                    _ => TextMode::Sensor(string(el, "sensorKey", "")),
                };
                Kind::Text(TextEl {
                    mode,
                    font_size: int(el, "fontSize", 32).max(1) as f32,
                    family: string(el, "fontFamily", "Consolas"),
                    bold: truthy(el, "bold", false),
                    color: color_opaque(el, "color", "#ffffffff"),
                    align: string(el, "align", "left"),
                    prefix: string(el, "prefix", ""),
                    unit: string(el, "unit", ""),
                })
            }
            "bar" => {
                let style = match string(el, "barStyle", "solid").as_str() {
                    "segmented" => BarStyle::Segmented {
                        segments: int_nonzero(el, "segmentCount", 12),
                        gap_pct: num(el, "segmentGap", 18.0) as f32 / 100.0,
                    },
                    "gapped" => BarStyle::Gapped {
                        segments: int_nonzero(el, "segmentCount", 16),
                        gap_px: int(el, "segmentGap", 2).max(1),
                    },
                    _ => BarStyle::Solid,
                };
                Kind::Bar(BarEl {
                    key: string(el, "sensorKey", "CPU_USAGE"),
                    max: num(el, "maxValue", 100.0),
                    radius: int(el, "cornerRadius", 0),
                    border: int(el, "borderThickness", 0),
                    bg: color(el, "bgColor", "#1a1a2299"),
                    fill: color(el, "fillColor", "#00b4ffff"),
                    border_color: color(el, "borderColor", "#00000000"),
                    style,
                })
            }
            "ring" => Kind::Ring(RingEl {
                key: string(el, "sensorKey", "CPU_USAGE"),
                max: num(el, "maxValue", 100.0),
                ring_width: int(el, "ringWidth", 14),
                arc: color_opaque(el, "arcColor", "#00b4ffff"),
                track: color_opaque(el, "trackColor", "#1a1a3399"),
                segmented: if string(el, "ringStyle", "solid") == "segmented" {
                    Some((int_nonzero(el, "segmentCount", 24), num(el, "segmentGap", 6.0) as f32))
                } else {
                    None
                },
                show_label: truthy(el, "showLabel", true),
                label_size: int(el, "labelFontSize", 28).max(1) as f32,
                label_family: string(el, "labelFontFamily", "Consolas"),
                label_bold: truthy(el, "labelBold", true),
                label_color: color_opaque(el, "labelColor", "#ffffffff"),
                unit: string(el, "unit", ""),
            }),
            "linegraph" => {
                let max1 = num(el, "maxValue", 100.0);
                let max2 = num(el, "maxValue2", 100.0);
                let mut series = vec![Series {
                    key: string(el, "sensorKey", ""),
                    color: color_opaque(el, "lineColor", "#00b4ffff"),
                    max: max1,
                    fill: match el.get("fillColor") {
                        Some(Value::String(s)) if !s.is_empty() => Some(parse_color(s)),
                        _ => None,
                    },
                }];
                for (k, c, d) in [("sensorKey2", "lineColor2", "#ff3df0ff"), ("sensorKey3", "lineColor3", "#5effc0ff")] {
                    if truthy(el, k, false) {
                        series.push(Series { key: string(el, k, ""), color: color_opaque(el, c, d), max: max2, fill: None });
                    }
                }
                Kind::Graph(GraphEl {
                    history_seconds: int(el, "historySeconds", 60),
                    series,
                    line_width: int(el, "lineWidth", 2).max(0) as f32,
                    radius: int(el, "cornerRadius", 4),
                    bg: color(el, "bgColor", "#0a0a1499"),
                    grid: color(el, "gridColor", "#ffffff22"),
                    show_grid: truthy(el, "showGrid", true),
                })
            }
            "rect" => Kind::Rect { fill: color(el, "fillColor", "#00b4ff99"), radius: int(el, "cornerRadius", 0) },
            "image" => {
                let mut bytes = None;
                if let Some(Value::String(du)) = el.get("data") {
                    bytes = decode_data_url(du).map(|(_, b)| b);
                }
                if bytes.is_none() {
                    if let (Some(z), Some(Value::String(f))) = (zip.as_mut(), el.get("filename")) {
                        bytes = read_zip_entry(z, f);
                    }
                }
                match bytes.map(|b| decode_resized(&b, w, h)) {
                    Some(Ok(p)) => Kind::Image(p),
                    Some(Err(e)) => {
                        crate::log_warn!("Image layer decode error: {e}");
                        continue;
                    }
                    None => continue,
                }
            }
            _ => continue,
        };
        elements.push(Element { id: string(el, "id", ""), x, y, w, h, kind });
    }

    // CUSTOM_* sensors picked in the Theme Builder: stored by LHM SensorId
    let mut custom_map = Vec::new();
    if let Some(Value::Object(m)) = o.get("sensorMap") {
        for (k, v) in m {
            if let (true, Value::String(sid)) = (k.starts_with("CUSTOM_"), v) {
                custom_map.push((k.clone(), sid.clone()));
            }
        }
    }

    // Fonts embedded in the theme
    let mut custom_fonts = Vec::new();
    if let Some(Value::Array(list)) = o.get("customFonts") {
        for cf in list.iter().filter_map(|c| c.as_object()) {
            let family = string(cf, "family", "");
            let data = string(cf, "data", "");
            if family.is_empty() || !data.contains(',') {
                continue;
            }
            match decode_data_url(&data).map(|(_, b)| ab_glyph::FontArc::try_from_vec(b)) {
                Some(Ok(f)) => {
                    crate::log_debug!("Custom font loaded: \"{family}\"");
                    custom_fonts.push((family, f));
                }
                Some(Err(e)) => crate::log_warn!("Custom font load error ({family}): {e}"),
                None => crate::log_warn!("Custom font extract error ({family}): invalid base64 data"),
            }
        }
    }

    Ok(Theme {
        name: string(o, "name", "?"),
        width,
        height,
        base,
        elements,
        custom_map,
        custom_fonts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colors() {
        assert_eq!(parse_color("#00b4ffff"), [0, 0xb4, 0xff, 0xff]);
        assert_eq!(parse_color("#112233"), [0x11, 0x22, 0x33, 255]);
        assert_eq!(parse_color("#1234"), [255, 255, 255, 255]);
        assert_eq!(parse_color("#zz2233"), [255, 255, 255, 255]);
    }
}

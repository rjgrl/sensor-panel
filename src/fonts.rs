//! Font lookup: fonts embedded in the theme first, then Windows' font folders
//! (system + per-user), then Consolas.

use ab_glyph::FontArc;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default)]
pub struct Fonts {
    /// family -> font embedded in the current theme (customFonts)
    custom: HashMap<String, FontArc>,
    /// (family, bold) -> resolved Windows font (None = not found)
    cache: HashMap<(String, bool), Option<FontArc>>,
    font_files: Option<Vec<PathBuf>>,
    /// loaded font files, so the same file is never held in memory twice
    by_path: HashMap<PathBuf, FontArc>,
}

const KNOWN: &[(&str, &str, &str)] = &[
    ("Consolas", "consolab.ttf", "consola.ttf"),
    ("Arial", "arialbd.ttf", "arial.ttf"),
    ("Segoe UI", "segoeuib.ttf", "segoeui.ttf"),
    ("Courier New", "courbd.ttf", "cour.ttf"),
    ("Tahoma", "tahomabd.ttf", "tahoma.ttf"),
    ("Verdana", "verdanab.ttf", "verdana.ttf"),
    ("Impact", "impact.ttf", "impact.ttf"),
    ("Georgia", "georgiab.ttf", "georgia.ttf"),
    ("Calibri", "calibrib.ttf", "calibri.ttf"),
    ("Times New Roman", "timesbd.ttf", "times.ttf"),
    ("Comic Sans MS", "comicbd.ttf", "comic.ttf"),
    ("Trebuchet MS", "trebucbd.ttf", "trebuc.ttf"),
    ("Palatino Linotype", "palabd.ttf", "pala.ttf"),
    ("Century Gothic", "gothicb.ttf", "gothic.ttf"),
];

fn font_dirs() -> Vec<PathBuf> {
    let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let mut v = vec![windir.join("Fonts")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
    }
    v
}

fn alnum_lower(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

fn load_file(p: &PathBuf) -> Option<FontArc> {
    let data = std::fs::read(p).ok()?;
    match FontArc::try_from_vec(data) {
        Ok(f) => Some(f),
        Err(e) => {
            crate::log_warn!("Font load error ({}): {e}", p.display());
            None
        }
    }
}

impl Fonts {
    /// Replace the theme-embedded fonts (customFonts)
    pub fn set_custom(&mut self, fonts: &[(String, FontArc)]) {
        self.custom = fonts.iter().cloned().collect();
    }

    fn all_font_files(&mut self) -> &Vec<PathBuf> {
        if self.font_files.is_none() {
            let mut v = Vec::new();
            for d in font_dirs() {
                if let Ok(rd) = std::fs::read_dir(&d) {
                    for e in rd.flatten() {
                        let p = e.path();
                        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                        if ext == "ttf" || ext == "otf" {
                            v.push(p);
                        }
                    }
                }
            }
            self.font_files = Some(v);
        }
        self.font_files.as_ref().unwrap()
    }

    fn find_windows_font(&mut self, family: &str, bold: bool) -> Option<PathBuf> {
        let dirs = font_dirs();
        if let Some((_, b, r)) = KNOWN.iter().find(|(f, _, _)| *f == family) {
            let (first, second) = if bold { (b, r) } else { (r, b) };
            for name in [first, second] {
                for d in &dirs {
                    let p = d.join(name);
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
        // Dynamic search: files whose name contains the family name
        let safe = alnum_lower(family);
        if safe.is_empty() {
            return None;
        }
        let files = self.all_font_files().clone();
        for d in &dirs {
            let mut best: Option<(i32, PathBuf)> = None;
            for p in files.iter().filter(|p| p.parent() == Some(d.as_path())) {
                let base = alnum_lower(&p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
                if base.contains(&safe) {
                    let is_bold = ["bold", "bd", "b"].iter().any(|x| base.contains(x));
                    let score = if is_bold == bold { 2 } else { 0 };
                    let better = match &best {
                        None => true,
                        Some((s, bp)) => score > *s || (score == *s && p > bp),
                    };
                    if better {
                        best = Some((score, p.clone()));
                    }
                }
            }
            if let Some((_, p)) = best {
                return Some(p);
            }
        }
        None
    }

    fn load_cached(&mut self, p: PathBuf) -> Option<FontArc> {
        if let Some(f) = self.by_path.get(&p) {
            return Some(f.clone());
        }
        let f = load_file(&p)?;
        self.by_path.insert(p, f.clone());
        Some(f)
    }

    pub fn get(&mut self, family: &str, bold: bool) -> Option<FontArc> {
        if let Some(f) = self.custom.get(family) {
            return Some(f.clone());
        }
        let key = (family.to_string(), bold);
        if let Some(f) = self.cache.get(&key) {
            return f.clone();
        }
        let mut font = self.find_windows_font(family, bold).and_then(|p| self.load_cached(p));
        if font.is_none() {
            let dirs = font_dirs();
            let fb = if bold { "consolab.ttf" } else { "consola.ttf" };
            if let Some(p) = dirs.iter().map(|d| d.join(fb)).find(|p| p.exists()) {
                font = self.load_cached(p);
            }
        }
        self.cache.insert(key, font.clone());
        font
    }
}

//! Persistent configuration (%APPDATA%\DS916Tray\config.json).
//!
//! Standard sensor keys are resolved by NAME on every read, so nothing about
//! sensors is cached here. CUSTOM_* sensors come from the loaded theme's own
//! sensorMap. Unknown / obsolete keys from older versions are dropped on load.

use serde::Serialize;
use serde_json::Value;

use crate::paths;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    pub com_port: String,
    pub fps: u32,
    pub theme_path: String,
    pub autostart: bool,
    /// LibreHardwareMonitor Remote Web Server host / port (LHM default is 8085)
    pub lhm_host: String,
    pub lhm_port: u16,
    /// empty = auto-detect active 3D app; or an exact exe name to pin a process
    pub rtss_process: String,
    /// "off", "normal" or "verbose"
    pub log_level: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            com_port: "COM3".into(),
            fps: 6,
            theme_path: String::new(),
            autostart: true,
            lhm_host: "127.0.0.1".into(),
            lhm_port: 8085,
            rtss_process: String::new(),
            log_level: "normal".into(),
        }
    }
}

fn get_str(o: &serde_json::Map<String, Value>, k: &str, d: &str) -> String {
    match o.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => d.to_string(),
    }
}

fn get_num(o: &serde_json::Map<String, Value>, k: &str, d: f64) -> f64 {
    match o.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(d),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(d),
        _ => d,
    }
}

fn get_bool(o: &serde_json::Map<String, Value>, k: &str, d: bool) -> bool {
    match o.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|v| v != 0.0).unwrap_or(d),
        _ => d,
    }
}

impl Config {
    pub fn load() -> Config {
        let d = Config::default();
        let text = match std::fs::read_to_string(paths::config_file()) {
            Ok(t) => t,
            Err(_) => return d,
        };
        let v: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(v) => v,
            Err(_) => return d,
        };
        let o = match v.as_object() {
            Some(o) => o,
            None => return d,
        };
        let fps = get_num(o, "fps", d.fps as f64);
        let port = get_num(o, "lhm_port", d.lhm_port as f64);
        Config {
            com_port: get_str(o, "com_port", &d.com_port),
            fps: if fps.is_finite() { fps.clamp(1.0, 30.0) as u32 } else { d.fps },
            theme_path: get_str(o, "theme_path", ""),
            autostart: get_bool(o, "autostart", true),
            lhm_host: get_str(o, "lhm_host", &d.lhm_host),
            lhm_port: if (1.0..=65535.0).contains(&port) { port as u16 } else { d.lhm_port },
            rtss_process: get_str(o, "rtss_process", ""),
            log_level: get_str(o, "log_level", &d.log_level),
        }
    }

    pub fn save(&self) {
        paths::ensure_dirs();
        match serde_json::to_string_pretty(self) {
            Ok(s) => {
                if let Err(e) = std::fs::write(paths::config_file(), s) {
                    crate::log_error!("Could not save config: {e}");
                }
            }
            Err(e) => crate::log_error!("Could not serialise config: {e}"),
        }
    }

    pub fn lhm_host_or_default(&self) -> String {
        let h = self.lhm_host.trim();
        if h.is_empty() { "127.0.0.1".into() } else { h.to_string() }
    }
}

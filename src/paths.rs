//! Everything the app uses lives under %APPDATA%\DS916Tray\ so the uninstaller
//! can clean it up in one place.

use std::path::PathBuf;

pub const APP_NAME: &str = "DS916Tray";

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(base).join(APP_NAME)
}

pub fn themes_dir() -> PathBuf {
    config_dir().join("Themes")
}
pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}
pub fn log_file() -> PathBuf {
    config_dir().join("ds916_tray_log.txt")
}
pub fn sensors_file() -> PathBuf {
    config_dir().join("lhm_sensors.json")
}
pub fn theme_builder_file() -> PathBuf {
    config_dir().join("theme_builder.html")
}

pub fn ensure_dirs() {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::create_dir_all(themes_dir());
}

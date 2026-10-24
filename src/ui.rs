//! Settings and Status windows.
//!
//! These run in their own short-lived process (`DS916Tray.exe --settings` /
//! `--status`), started from the tray menu. The background tray process never
//! loads a GUI toolkit, so its memory stays low; the window's memory is
//! returned to Windows as soon as it's closed. Saving settings writes
//! config.json and tells the running tray app to reload it.

use eframe::egui::{self, Color32, RichText};
use std::collections::HashMap;

use crate::config::Config;
use crate::winutil::{self, Icon};
use crate::{autostart, device, lhm, logging, paths, rtss, theme};

const BG: Color32 = Color32::from_rgb(0x18, 0x18, 0x1c);
const PANEL: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x25);
const ACC: Color32 = Color32::from_rgb(0x00, 0xb4, 0xff);
const GRN: Color32 = Color32::from_rgb(0x4f, 0xc8, 0x7a);
const RED: Color32 = Color32::from_rgb(0xe0, 0x5a, 0x4b);
const YEL: Color32 = Color32::from_rgb(0xd4, 0xb8, 0x4a);
const MUT: Color32 = Color32::from_rgb(0x88, 0x88, 0x88);
const TXT: Color32 = Color32::from_rgb(0xe8, 0xe6, 0xdf);

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = PANEL;
    v.override_text_color = Some(TXT);
    v.selection.bg_fill = Color32::from_rgb(0x00, 0x5a, 0x80);
    ctx.set_visuals(v);
}

fn options(title: &str, w: f32, h: f32) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([w, h])
            .with_min_inner_size([w.min(500.0), 360.0]),
        centered: true,
        ..Default::default()
    }
}

/// Tell the running tray app to reload config.json
fn notify_tray_reload() {
    if let Some(h) = winutil::find_window(Some(winutil::TRAY_CLASS), None) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(h, winutil::WM_APP_RELOAD_CONFIG, 0, 0);
        }
    }
}

/// None = tray app not running; Some(0 stopped / 1 waiting for device / 2 streaming)
fn query_tray_state() -> Option<u8> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, SMTO_ABORTIFHUNG};
    let h = winutil::find_window(Some(winutil::TRAY_CLASS), None)?;
    let mut result: usize = 0;
    let ok = unsafe { SendMessageTimeoutW(h, winutil::WM_APP_QUERY_STATE, 0, 0, SMTO_ABORTIFHUNG, 1000, &mut result) };
    if ok == 0 { None } else { Some(result as u8) }
}

/// Close this window process as soon as the tray app that opened it exits —
/// whether via Exit, Uninstall, a crash or Task Manager's "End task".
pub fn watch_parent(pid: u32) {
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};
    std::thread::spawn(move || unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !winutil::is_null(h) {
            WaitForSingleObject(h, INFINITE);
            std::process::exit(0);
        }
    });
}

// ── Settings ────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    General,
    Lhm,
    Rtss,
}

struct Settings {
    tab: Tab,
    orig: Config,
    ports: Vec<String>,
    com_port: String,
    toast: Option<(String, Color32)>,
    fps: u32,
    theme_path: String,
    autostart: bool,
    log_level: String,
    lhm_host: String,
    lhm_port: String,
    lhm_status: (String, Color32),
    rtss_manual: bool,
    rtss_proc: String,
    rtss_apps: Vec<String>,
    rtss_status: (String, Color32),
}

impl Settings {
    fn new() -> Self {
        let c = Config::load();
        let mut s = Settings {
            tab: Tab::General,
            ports: device::list_ports(),
            com_port: c.com_port.clone(),
            toast: None,
            fps: c.fps,
            theme_path: c.theme_path.clone(),
            autostart: c.autostart,
            log_level: c.log_level.clone(),
            lhm_host: c.lhm_host.clone(),
            lhm_port: c.lhm_port.to_string(),
            lhm_status: (String::new(), MUT),
            rtss_manual: !c.rtss_process.trim().is_empty(),
            rtss_proc: c.rtss_process.clone(),
            rtss_apps: Vec::new(),
            rtss_status: ("Checking...".into(), MUT),
            orig: c,
        };
        if !s.ports.contains(&s.com_port) && !s.com_port.is_empty() {
            s.ports.insert(0, s.com_port.clone());
        }
        s.test_lhm();
        s.refresh_rtss();
        s
    }

    fn host(&self) -> String {
        let h = self.lhm_host.trim();
        if h.is_empty() { "127.0.0.1".into() } else { h.to_string() }
    }

    fn port(&self) -> u16 {
        self.lhm_port.trim().parse().ok().filter(|p| *p > 0).unwrap_or(8085)
    }

    fn test_lhm(&mut self) {
        // Test with the values currently typed in, without saving them yet
        self.lhm_status = match lhm::fetch_once(&self.host(), self.port()) {
            Ok((sensors, version)) => (
                format!(
                    "✅ Connected — LHM {}, {} sensors, {} standard keys mapped",
                    version.unwrap_or_default(),
                    sensors.len(),
                    lhm::resolve_standard(&sensors).len()
                ),
                GRN,
            ),
            Err(_) => (
                "○ Not reachable — is LibreHardwareMonitor running with\n   Options → Remote Web Server → Run enabled?".into(),
                RED,
            ),
        };
    }

    fn refresh_rtss(&mut self) {
        let (connected, apps) = rtss::list_apps();
        let mut names: Vec<String> = apps.iter().map(|(_, n, _)| n.clone()).collect();
        names.sort();
        names.dedup();
        self.rtss_status = if !apps.is_empty() {
            (format!("✅ RTSS connected — {} active 3D app(s) detected", apps.len()), GRN)
        } else if connected {
            ("✅ RTSS connected — no active 3D app detected right now".into(), GRN)
        } else {
            ("○ RTSS not running (optional — install from guru3d.com if you want FPS)".into(), MUT)
        };
        self.rtss_apps = names;
    }

    fn save(&mut self) {
        let old = self.orig.clone();
        let new = Config {
            com_port: self.com_port.clone(),
            fps: self.fps.clamp(1, 30),
            theme_path: self.theme_path.trim().to_string(),
            autostart: self.autostart,
            lhm_host: self.host(),
            lhm_port: self.port(),
            rtss_process: if self.rtss_manual { self.rtss_proc.trim().to_string() } else { String::new() },
            log_level: self.log_level.clone(),
        };
        // Log what actually changed (before a possible switch to 'off')
        let changes: [(&str, String, String); 7] = [
            ("com_port", old.com_port.clone(), new.com_port.clone()),
            ("fps", old.fps.to_string(), new.fps.to_string()),
            ("autostart", old.autostart.to_string(), new.autostart.to_string()),
            ("rtss_process", old.rtss_process.clone(), new.rtss_process.clone()),
            ("lhm_host", old.lhm_host.clone(), new.lhm_host.clone()),
            ("lhm_port", old.lhm_port.to_string(), new.lhm_port.to_string()),
            ("log_level", old.log_level.clone(), new.log_level.clone()),
        ];
        for (k, a, b) in changes.iter() {
            if a != b {
                crate::log_info!("Setting changed: {k} = {a:?} -> {b:?}");
            }
        }
        new.save();
        logging::set_level(&new.log_level);
        autostart::set_autostart(new.autostart);
        notify_tray_reload();
        self.orig = new;
    }

    fn general(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("general").num_columns(3).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("COM Port:");
            egui::ComboBox::new("com_port", "").selected_text(self.com_port.clone()).show_ui(ui, |ui| {
                for p in self.ports.clone() {
                    ui.selectable_value(&mut self.com_port, p.clone(), p);
                }
            });
            if ui.button("Auto-detect").clicked() {
                match device::detect_ds916_port() {
                    Some(found) => {
                        if !self.ports.contains(&found) {
                            self.ports.push(found.clone());
                        }
                        self.toast = Some((format!("✅ DS916 found on {found}"), GRN));
                        self.com_port = found;
                    }
                    None => self.toast = Some(("DS916 not found — is it plugged in?".into(), RED)),
                }
            }
            ui.end_row();

            ui.label("");
            match &self.toast {
                Some((t, c)) => ui.label(RichText::new(t).small().color(*c)),
                None => ui.label(""),
            };
            ui.label("");
            ui.end_row();

            ui.label("FPS:");
            ui.add(egui::Slider::new(&mut self.fps, 1..=30));
            ui.label("");
            ui.end_row();

            ui.label("Theme File:");
            ui.add(egui::TextEdit::singleline(&mut self.theme_path).desired_width(260.0));
            if ui.button("Browse…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("DS916 Theme", &["ds916theme", "zip"]).pick_file() {
                    self.theme_path = p.display().to_string();
                }
            }
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.checkbox(&mut self.autostart, "Start display automatically with Windows");
        if self.orig.autostart && !autostart::is_autostart() {
            ui.label(
                RichText::new("Windows startup entry is missing or disabled — click Save to repair it.")
                    .small()
                    .color(YEL),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Logging:");
            egui::ComboBox::new("log_level", "").selected_text(self.log_level.clone()).show_ui(ui, |ui| {
                for l in ["off", "normal", "verbose"] {
                    ui.selectable_value(&mut self.log_level, l.to_string(), l);
                }
            });
        });
        ui.label(
            RichText::new("verbose = detailed per-frame/per-read diagnostics, for troubleshooting")
                .small()
                .color(Color32::from_rgb(0x66, 0x66, 0x66)),
        );
        ui.add_space(6.0);
        if ui.button("📁 Open Log Folder").clicked() && !winutil::shell_open(&paths::config_dir().display().to_string()) {
            winutil::message_box("DS916", "Could not open the log folder.", Icon::Error);
        }
    }

    fn lhm_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Sensor Source — LibreHardwareMonitor").strong().color(ACC));
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "All hardware sensors are read from LibreHardwareMonitor (LHM) through its\n\
                 built-in web server. One-time setup in LHM:\n  \
                 1. Run LibreHardwareMonitor as administrator\n  \
                 2. Options → Remote Web Server → Run   (default port 8085)\n  \
                 3. Optional: Options → Run On Windows Startup, Start Minimized,\n     \
                 Minimize To Tray — so it's always there in the background\n\n\
                 There is no time limit and nothing to restart — LHM can run indefinitely.",
            )
            .color(Color32::from_rgb(0xaa, 0xaa, 0xaa)),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Host:");
            ui.add(egui::TextEdit::singleline(&mut self.lhm_host).desired_width(140.0));
            ui.label("Port:");
            ui.add(egui::TextEdit::singleline(&mut self.lhm_port).desired_width(60.0));
        });
        ui.label(RichText::new("Leave as 127.0.0.1 : 8085 unless you changed the port in LHM.").small().color(MUT));
        ui.add_space(8.0);
        let (t, c) = self.lhm_status.clone();
        ui.label(RichText::new(t).color(c));
        ui.add_space(4.0);
        if ui.button("↻ Test Connection").clicked() {
            self.test_lhm();
        }
    }

    fn rtss_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(
                "RivaTuner Statistics Server (RTSS) is an optional, separate source for\n\
                 reliable per-game FPS — independent of LibreHardwareMonitor. If RTSS isn't installed\n\
                 or running, the FPS sensor simply stays unavailable; everything else\n\
                 keeps working normally.",
            )
            .color(MUT),
        );
        ui.add_space(8.0);
        let (t, c) = self.rtss_status.clone();
        ui.label(RichText::new(t).color(c));
        ui.add_space(8.0);
        ui.radio_value(&mut self.rtss_manual, false, "Auto-detect active 3D app (recommended)");
        ui.radio_value(&mut self.rtss_manual, true, "Pin a specific process:");
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.add_enabled_ui(self.rtss_manual, |ui| {
                let shown = if self.rtss_proc.is_empty() { "(choose)".to_string() } else { self.rtss_proc.clone() };
                egui::ComboBox::new("rtss_proc", "").width(220.0).selected_text(shown).show_ui(ui, |ui| {
                    for n in self.rtss_apps.clone() {
                        ui.selectable_value(&mut self.rtss_proc, n.clone(), n);
                    }
                });
            });
            if ui.button("↻ Refresh List").clicked() {
                self.refresh_rtss();
            }
        });
    }
}

impl eframe::App for Settings {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::bottom("buttons").show(ctx, |ui| {
            ui.add_space(6.0);
            // horizontal() keeps the bar one row high (a bare right_to_left
            // layout would take the whole window height)
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Save").clicked() {
                        self.save();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (t, label) in [(Tab::General, "  General  "), (Tab::Lhm, "  LibreHardwareMonitor  "), (Tab::Rtss, "  RTSS (FPS)  ")] {
                    let text = if self.tab == t { RichText::new(label).color(ACC).strong() } else { RichText::new(label).color(MUT) };
                    if ui.selectable_label(self.tab == t, text).clicked() {
                        self.tab = t;
                    }
                }
            });
            ui.separator();
            ui.add_space(6.0);
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::General => self.general(ui),
                Tab::Lhm => self.lhm_tab(ui),
                Tab::Rtss => self.rtss_tab(ui),
            });
        });
    }
}

pub fn run_settings() {
    if let Some(h) = winutil::find_window(None, Some(winutil::SETTINGS_TITLE)) {
        winutil::bring_to_front(h);
        return;
    }
    let r = eframe::run_native(
        winutil::SETTINGS_TITLE,
        options(winutil::SETTINGS_TITLE, 600.0, 560.0),
        Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(Settings::new()))
        }),
    );
    if let Err(e) = r {
        crate::log_error!("Settings window error: {e}");
        winutil::message_box("DS916", &format!("Could not open the Settings window:\n{e}"), Icon::Error);
    }
}

// ── Status ──────────────────────────────────────────────────────────────────

struct StatusData {
    tray: Option<u8>,
    cfg: Config,
    theme: Option<(String, u32, u32)>,
    lhm: Result<(usize, usize, Option<String>), String>,
    snapshot: Vec<(&'static str, Option<f64>, &'static str)>,
    autostart: bool,
    rtss: bool,
}

fn collect_status() -> StatusData {
    let cfg = Config::load();
    let theme = if cfg.theme_path.is_empty() { None } else { theme::peek_info(std::path::Path::new(&cfg.theme_path)) };
    let mut values = HashMap::new();
    let lhm = match lhm::fetch_once(&cfg.lhm_host_or_default(), cfg.lhm_port) {
        Ok((sensors, version)) => {
            let std_n = lhm::resolve_standard(&sensors).len();
            lhm::read_values(&sensors, &[], None, &mut values);
            Ok((sensors.len(), std_n, version))
        }
        Err(e) => Err(e),
    };
    let snapshot = vec![
        ("CPU Usage", values.get("CPU_USAGE").copied(), "%"),
        ("CPU Temp", values.get("CPU_TEMP").copied(), "°C"),
        ("GPU Usage", values.get("GPU_USAGE").copied(), "%"),
        ("GPU Temp", values.get("GPU_TEMP").copied(), "°C"),
        ("MB Temp", values.get("MB_TEMP").copied(), "°C"),
        ("CPU Fan", values.get("CPU_FAN").copied(), "RPM"),
    ];
    StatusData {
        tray: query_tray_state(),
        theme,
        lhm,
        snapshot,
        autostart: autostart::is_autostart(),
        rtss: rtss::list_apps().0,
        cfg,
    }
}

struct Status {
    data: StatusData,
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none().fill(PANEL).inner_margin(egui::Margin::same(10.0)).rounding(4.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong().color(ACC));
        egui::Grid::new(title).num_columns(2).spacing([12.0, 3.0]).show(ui, body);
    });
    ui.add_space(6.0);
}

fn row(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    ui.label(RichText::new(label).color(Color32::from_rgb(0x66, 0x66, 0x66)));
    ui.label(RichText::new(value).strong().color(color));
    ui.end_row();
}

impl eframe::App for Status {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let d = &self.data;
        let streaming = d.tray == Some(2);
        let running = matches!(d.tray, Some(1) | Some(2));
        egui::TopBottomPanel::top("hdr").frame(egui::Frame::none().fill(ACC).inner_margin(egui::Margin::symmetric(12.0, 8.0))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("DS916 Screen Manager").strong().size(15.0).color(Color32::BLACK));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (t, c) = if running { ("Running", Color32::from_rgb(0x00, 0x30, 0x20)) } else { ("Stopped", Color32::from_rgb(0x30, 0, 0)) };
                    ui.label(RichText::new(t).strong().color(c));
                });
            });
        });
        let mut refresh = false;
        egui::TopBottomPanel::bottom("btns").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("↻ Refresh").color(ACC)).clicked() {
                    refresh = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                section(ui, "Display", |ui| {
                    let (t, c) = match d.tray {
                        None => ("○ Tray app is not running", RED),
                        Some(0) => ("○ Stopped", RED),
                        Some(1) => ("⚠ Waiting for the DS916 (not detected yet)", YEL),
                        _ => ("✔ Streaming to screen", GRN),
                    };
                    let _ = streaming;
                    row(ui, "Status", t, c);
                    row(ui, "COM Port", &d.cfg.com_port, TXT);
                    row(ui, "Target FPS", &d.cfg.fps.to_string(), TXT);
                    match &d.theme {
                        Some((n, w, h)) => {
                            row(ui, "Theme", n, TXT);
                            row(ui, "Resolution", &format!("{w}×{h}"), TXT);
                        }
                        None => {
                            row(ui, "Theme", "—", TXT);
                            row(ui, "Resolution", "—", TXT);
                        }
                    }
                });
                section(ui, "Sensor Source — LibreHardwareMonitor", |ui| match &d.lhm {
                    Ok((n, s, v)) => {
                        row(ui, "Source", &format!("Web server http://{}:{}  ✅", d.cfg.lhm_host_or_default(), d.cfg.lhm_port), GRN);
                        row(ui, "Version", v.as_deref().unwrap_or("unknown"), TXT);
                        row(ui, "Sensors", &format!("{n} found, {s} standard keys mapped"), TXT);
                    }
                    Err(_) => row(ui, "Source", "Unavailable — LibreHardwareMonitor not running or Remote Web Server not enabled", YEL),
                });
                section(ui, "Live Sensor Snapshot", |ui| {
                    for (l, v, u) in &d.snapshot {
                        match v {
                            Some(v) => row(ui, l, &format!("{v:.1} {u}"), TXT),
                            None => row(ui, l, "— (not mapped)", Color32::from_rgb(0x66, 0x66, 0x66)),
                        }
                    }
                });
                section(ui, "System", |ui| {
                    if d.autostart {
                        row(ui, "Windows Startup", "✅ Enabled", GRN);
                    } else {
                        row(ui, "Windows Startup", "○ Disabled", Color32::from_rgb(0x66, 0x66, 0x66));
                    }
                    row(ui, "RTSS (FPS)", if d.rtss { "✅ Connected" } else { "○ Not running (optional)" }, if d.rtss { GRN } else { MUT });
                });
            });
        });
        if refresh {
            self.data = collect_status();
        }
    }
}

pub fn run_status() {
    if let Some(h) = winutil::find_window(None, Some(winutil::STATUS_TITLE)) {
        winutil::bring_to_front(h);
        return;
    }
    let data = collect_status();
    let r = eframe::run_native(
        winutil::STATUS_TITLE,
        options(winutil::STATUS_TITLE, 460.0, 600.0),
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(Status { data }))
        }),
    );
    if let Err(e) = r {
        crate::log_error!("Status window error: {e}");
        winutil::message_box("DS916", &format!("Could not open the Status window:\n{e}"), Icon::Error);
    }
}

//! Shared tray-process state and the display (streaming) thread.

use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::render::Renderer;
use crate::theme::Theme;
use crate::{device, lhm, protocol, sensors, theme, winutil};

/// Display link state (also returned to the Status window)
pub const LINK_STOPPED: u8 = 0;
pub const LINK_WAITING: u8 = 1; // display on, waiting for the DS916 to appear
pub const LINK_STREAMING: u8 = 2;

/// How often to look for the DS916 while it isn't connected (e.g. right after
/// boot, before Windows has enumerated the USB device, or after it was unplugged)
const PORT_RETRY: Duration = Duration::from_secs(3);

pub struct App {
    pub cfg: Mutex<Config>,
    pub theme: Mutex<Option<Arc<Theme>>>,
    pub theme_path: Mutex<String>,
    theme_gen: AtomicU64,
    pub running: AtomicBool,
    pub link: AtomicU8,
    reconnect: AtomicBool,
    discovered: AtomicBool,
    /// tray window handle, for posting state-change notifications
    pub hwnd: AtomicUsize,
    stream: Mutex<Option<JoinHandle<()>>>,
}

static APP: OnceLock<App> = OnceLock::new();

pub fn init(cfg: Config) {
    let _ = APP.set(App {
        cfg: Mutex::new(cfg),
        theme: Mutex::new(None),
        theme_path: Mutex::new(String::new()),
        theme_gen: AtomicU64::new(0),
        running: AtomicBool::new(false),
        link: AtomicU8::new(LINK_STOPPED),
        reconnect: AtomicBool::new(false),
        discovered: AtomicBool::new(false),
        hwnd: AtomicUsize::new(0),
        stream: Mutex::new(None),
    });
}

pub fn app() -> &'static App {
    APP.get().expect("app::init not called")
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

pub fn cfg() -> Config {
    lock(&app().cfg).clone()
}

fn notify_state_changed() {
    let h = app().hwnd.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                winutil::usize_to_hwnd(h),
                winutil::WM_APP_STATE_CHANGED,
                0,
                0,
            );
        }
    }
}

fn set_link(v: u8) {
    if app().link.swap(v, Ordering::Relaxed) != v {
        notify_state_changed();
    }
}

pub fn is_running() -> bool {
    app().running.load(Ordering::Relaxed)
}

pub fn theme_name() -> Option<String> {
    lock(&app().theme).as_ref().map(|t| t.name.clone())
}

// ── theme ───────────────────────────────────────────────────────────────────

pub fn load_theme(path: &str) -> bool {
    match theme::load(Path::new(path)) {
        Ok(t) => {
            let name = t.name.clone();
            *lock(&app().theme) = Some(Arc::new(t));
            *lock(&app().theme_path) = path.to_string();
            app().theme_gen.fetch_add(1, Ordering::Relaxed);
            {
                let mut c = lock(&app().cfg);
                c.theme_path = path.to_string();
                c.save();
            }
            crate::log_info!("Theme loaded: {name}");
            notify_state_changed();
            true
        }
        Err(e) => {
            crate::log_error!("Theme load error: {e}");
            false
        }
    }
}

// ── sensor discovery ────────────────────────────────────────────────────────

/// Fetch from LHM now, write lhm_sensors.json and refresh the VRAM fallback.
pub fn run_discovery() -> Option<(std::path::PathBuf, usize)> {
    let c = cfg();
    let snap = lhm::refresh(&c.lhm_host_or_default(), c.lhm_port);
    let version = {
        let mut l = lhm::lock();
        l.newly_connected = false;
        l.version.clone()
    };
    let snap = match snap {
        Some(s) => s,
        None => {
            crate::log_warn!("Cannot discover sensors - LibreHardwareMonitor not reachable");
            return None;
        }
    };
    let res = lhm::discover_sensors(&snap, version);
    let vram = lhm::detect_gpu_vram_capacity(&snap);
    lhm::lock().detected_vram_gb = vram;
    if res.is_some() {
        app().discovered.store(true, Ordering::Relaxed);
    }
    res
}

// ── LibreHardwareMonitor poller ─────────────────────────────────────────────

/// Fetch sensor data from LHM on its own thread, so a slow or briefly
/// unresponsive LHM never holds up a frame. The display thread just uses the
/// latest snapshot (see lhm::Lhm::current). Only polls while the display runs.
pub fn start_lhm_poller() {
    let r = std::thread::Builder::new().name("lhm".into()).stack_size(1024 * 1024).spawn(|| loop {
        if !is_running() {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let t0 = Instant::now();
        let c = cfg();
        lhm::refresh(&c.lhm_host_or_default(), c.lhm_port);
        let failing = lhm::lock().failing_for();
        // LHM refreshes about once a second; 0.5s keeps the panel at most one
        // LHM update behind. While it isn't answering, retry every second
        // during a short hiccup, then back off to every 5 seconds.
        let wait = match failing {
            None => Duration::from_millis(500),
            Some(d) if d < lhm::STALE_LIMIT => Duration::from_secs(1),
            Some(_) => Duration::from_secs(5),
        };
        std::thread::sleep(wait.saturating_sub(t0.elapsed()));
    });
    if let Err(e) = r {
        crate::log_error!("Could not start the LibreHardwareMonitor poller: {e}");
    }
}

// ── display thread ──────────────────────────────────────────────────────────

pub fn start_saved_display() -> bool {
    let path = cfg().theme_path;
    if path.is_empty() {
        return false;
    }
    if !Path::new(&path).exists() {
        crate::log_warn!("Last theme not found: {path}");
        return false;
    }
    if load_theme(&path) {
        start_display();
    }
    is_running()
}

pub fn start_display() {
    let a = app();
    let mut slot = lock(&a.stream);
    if a.running.load(Ordering::Relaxed) {
        return;
    }
    if let Some(h) = slot.take() {
        let _ = h.join();
    }
    a.running.store(true, Ordering::Relaxed);
    set_link(LINK_WAITING);
    match std::thread::Builder::new().name("display".into()).stack_size(1024 * 1024).spawn(stream_loop) {
        Ok(h) => {
            *slot = Some(h);
            crate::log_info!("Display started");
        }
        Err(e) => {
            a.running.store(false, Ordering::Relaxed);
            set_link(LINK_STOPPED);
            crate::log_error!("Could not start display thread: {e}");
        }
    }
    notify_state_changed();
}

pub fn stop_display() {
    let a = app();
    let was = a.running.swap(false, Ordering::Relaxed);
    if let Some(h) = lock(&a.stream).take() {
        let _ = h.join();
    }
    set_link(LINK_STOPPED);
    if was {
        crate::log_info!("Display stopped");
    }
    notify_state_changed();
}

/// Ask the display thread to close and re-open the COM port (e.g. port changed in Settings)
pub fn request_reconnect() {
    app().reconnect.store(true, Ordering::Relaxed);
}

/// Sleep up to `d`, waking early if the display is stopped
fn nap(d: Duration) {
    let end = Instant::now() + d;
    while is_running() {
        let now = Instant::now();
        if now >= end {
            break;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
}

fn open_port(logged_fail: &mut bool) -> Option<Box<dyn serialport::SerialPort>> {
    // Auto-detect the DS916; fall back to the configured port
    let detected = device::detect_ds916_port();
    let port_name = {
        let mut c = lock(&app().cfg);
        if let Some(d) = &detected {
            if *d != c.com_port {
                crate::log_info!("Updating COM port: {} -> {d}", c.com_port);
                c.com_port = d.clone();
                c.save();
            }
        }
        c.com_port.clone()
    };
    match device::open(&port_name) {
        Ok(p) => {
            match &detected {
                Some(_) => crate::log_info!("Opened {port_name} (DS916 auto-detected, VID=33C3 PID=F101)"),
                None => crate::log_info!("Opened {port_name}"),
            }
            *logged_fail = false;
            Some(p)
        }
        Err(e) => {
            if !*logged_fail {
                crate::log_warn!(
                    "Port error on {port_name}: {e} -- the DS916 may not be ready yet (normal right after boot) \
                     or is unplugged; retrying every {}s",
                    PORT_RETRY.as_secs()
                );
                *logged_fail = true;
            } else {
                crate::log_debug!("Port {port_name} still unavailable: {e}");
            }
            None
        }
    }
}

fn encode_jpeg(pm: &tiny_skia::Pixmap, out: &mut Vec<u8>) -> Result<(), String> {
    out.clear();
    let mut enc = jpeg_encoder::Encoder::new(&mut *out, 88);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
    // The frame is fully opaque, so premultiplied == straight RGBA
    enc.encode(pm.data(), pm.width() as u16, pm.height() as u16, jpeg_encoder::ColorType::Rgba)
        .map_err(|e| e.to_string())
}

fn stream_loop() {
    let a = app();
    let mut renderer = Renderer::new();
    let mut gen_seen = u64::MAX;
    let mut port: Option<Box<dyn serialport::SerialPort>> = None;
    let mut first = true;
    let mut last_attempt: Option<Instant> = None;
    let mut logged_fail = false;
    let mut jpeg = Vec::with_capacity(512 * 1024);
    let mut frame = Vec::with_capacity(512 * 1024);
    let mut frames: u64 = 0;

    while a.running.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        let cfg = cfg();
        let interval = Duration::from_secs_f64(1.0 / cfg.fps.max(1) as f64);

        if a.reconnect.swap(false, Ordering::Relaxed) && port.is_some() {
            crate::log_info!("Re-opening the COM port");
            port = None;
            last_attempt = None;
        }

        // 1. Make sure the DS916 is connected (keep retrying — never give up)
        if port.is_none() {
            let due = last_attempt.map(|t| t.elapsed() >= PORT_RETRY).unwrap_or(true);
            if due {
                last_attempt = Some(Instant::now());
                port = open_port(&mut logged_fail);
                if port.is_some() {
                    first = true;
                    set_link(LINK_STREAMING);
                } else {
                    set_link(LINK_WAITING);
                }
            }
            if port.is_none() {
                nap(Duration::from_millis(250));
                continue;
            }
        }

        // 2. Theme
        let theme = lock(&a.theme).clone();
        let theme = match theme {
            Some(t) => t,
            None => {
                nap(Duration::from_millis(500));
                continue;
            }
        };
        let gen = a.theme_gen.load(Ordering::Relaxed);
        if gen != gen_seen {
            renderer.reset(&theme.custom_fonts);
            gen_seen = gen;
        }

        // 3. Sensors -> render -> JPEG (panics are caught so one bad frame
        //    can't take the whole app down)
        let res = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
            let values = sensors::read_all(&cfg, &theme.custom_map);
            let pm = renderer.render(&theme, &values, cfg.fps);
            encode_jpeg(pm, &mut jpeg)
        }));
        match res {
            Ok(Ok(())) => {
                protocol::make_frame(&jpeg, first, &mut frame);
                if let Some(p) = port.as_mut() {
                    let w = p.write_all(&frame).and_then(|_| p.flush());
                    match w {
                        Ok(()) => {
                            if first {
                                crate::log_info!("First frame sent to DS916 ({} KB JPEG)", jpeg.len() / 1024);
                            }
                            first = false;
                            frames += 1;
                            if frames % 600 == 1 {
                                crate::log_debug!("Frame {frames}: {} KB JPEG in {} ms", jpeg.len() / 1024, t0.elapsed().as_millis());
                            }
                        }
                        Err(e) => {
                            crate::log_warn!("DS916 write failed ({e}) - reconnecting");
                            port = None;
                            last_attempt = None;
                            logged_fail = false;
                            set_link(LINK_WAITING);
                            nap(Duration::from_secs(1));
                            continue;
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                crate::log_error!("Stream error: {e}");
                nap(Duration::from_secs(1));
            }
            Err(_) => {
                crate::log_error!("Stream error: renderer crashed on this frame - resetting renderer");
                renderer = Renderer::new();
                gen_seen = u64::MAX;
                nap(Duration::from_secs(1));
            }
        }

        // 4. LHM came up after we started (e.g. at boot): run discovery once
        let newly = std::mem::take(&mut lhm::lock().newly_connected);
        if newly && !a.discovered.load(Ordering::Relaxed) {
            run_discovery();
        }

        let el = t0.elapsed();
        if el < interval {
            nap(interval - el);
        }
    }
    drop(port);
}

/// Render the configured theme once with live sensor values and save it as PNG.
pub fn render_preview(out: &str) -> bool {
    let c = cfg();
    let t = match theme::load(Path::new(&c.theme_path)) {
        Ok(t) => t,
        Err(e) => {
            crate::log_error!("Preview: theme load error: {e}");
            return false;
        }
    };
    let mut r = Renderer::new();
    r.reset(&t.custom_fonts);
    lhm::refresh(&c.lhm_host_or_default(), c.lhm_port);
    let values = sensors::read_all(&c, &t.custom_map);
    // a few frames so line graphs have some history
    for _ in 0..3 {
        r.render(&t, &values, c.fps);
    }
    let pm = r.render(&t, &values, c.fps);
    let img = match image::RgbaImage::from_raw(pm.width(), pm.height(), pm.data().to_vec()) {
        Some(i) => i,
        None => return false,
    };
    match img.save(out) {
        Ok(()) => true,
        Err(e) => {
            crate::log_error!("Preview: could not save {out}: {e}");
            false
        }
    }
}

// ── settings reload (posted by the Settings window after Save) ──────────────

pub fn reload_config() {
    let new = Config::load();
    let old = {
        let mut c = lock(&app().cfg);
        std::mem::replace(&mut *c, new.clone())
    };
    crate::logging::set_level(&new.log_level);
    if (old.lhm_host.as_str(), old.lhm_port) != (new.lhm_host.as_str(), new.lhm_port) {
        lhm::refresh(&new.lhm_host_or_default(), new.lhm_port);
    }
    if old.com_port != new.com_port {
        request_reconnect();
    }
    let current = lock(&app().theme_path).clone();
    if !new.theme_path.is_empty() && new.theme_path != current {
        load_theme(&new.theme_path);
    }
    notify_state_changed();
}

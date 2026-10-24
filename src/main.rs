//! DS916 Tray — runs in the Windows system tray and streams themes to the
//! Jonsbo DS916 screen, with sensor values from LibreHardwareMonitor.
//!
//!   DS916Tray.exe              start the tray app (the normal way)
//!   DS916Tray.exe --autostart  same, used by the Windows startup entry
//!   DS916Tray.exe --settings   Settings window (opened from the tray menu)
//!   DS916Tray.exe --status     Status window   (opened from the tray menu)
//!   DS916Tray.exe --boot       display only, before sign-in (scheduled task)

#![windows_subsystem = "windows"]

mod app;
mod autostart;
mod boot;
mod config;
mod device;
mod fonts;
mod http;
mod lhm;
mod logging;
mod paths;
mod protocol;
mod render;
mod rtss;
mod sensors;
mod theme;
mod tray;
mod ui;
mod winutil;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;

fn main() {
    paths::ensure_dirs();
    let cfg = config::Config::load();
    logging::set_level(&cfg.log_level);

    std::panic::set_hook(Box::new(|info| {
        crate::log_error!("Unexpected internal error: {info}");
    }));

    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |a: &str| args.iter().any(|x| x.eq_ignore_ascii_case(a));

    if has("--boot") {
        logging::use_boot_log();
        if args.len() != 1 {
            log_error!("--boot cannot be combined with other arguments");
            std::process::exit(1);
        }
        set_working_directory();
        if let Err(e) = boot::run(cfg) {
            log_error!("Boot display: {e}");
            std::process::exit(1);
        }
        return;
    }
    // Used by the setup script; reuse registry handling without starting a tray.
    if has("--register-autostart") {
        if args.len() != 1 || !cfg.autostart {
            std::process::exit(1);
        }
        autostart::clear_run_as_admin_flag();
        autostart::set_autostart(true);
        std::process::exit(if autostart::is_autostart() { 0 } else { 1 });
    }

    // Settings/Status windows opened from the tray close when the tray exits
    if let Some(pid) = args
        .iter()
        .position(|a| a.eq_ignore_ascii_case("--parent"))
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u32>().ok())
    {
        ui::watch_parent(pid);
    }
    if has("--settings") {
        ui::run_settings();
        return;
    }
    if has("--status") {
        ui::run_status();
        return;
    }
    // DS916Tray.exe --preview out.png : render one frame of the current theme
    // with live sensor values to a PNG (for checking a theme without the screen)
    if let Some(i) = args.iter().position(|a| a.eq_ignore_ascii_case("--preview")) {
        let out = args.get(i + 1).cloned().unwrap_or_else(|| "preview.png".into());
        app::init(cfg);
        std::process::exit(if app::render_preview(&out) { 0 } else { 1 });
    }
    let at_logon = has(autostart::AUTOSTART_ARG);

    // Single instance: the Run key, a shortcut and build.bat could all start it
    let name = winutil::wide("Local\\DS916Tray_SingleInstance");
    let _mutex = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if !at_logon {
            // Started again by hand: show the running instance's status instead
            tray::spawn_window("--status", winutil::STATUS_TITLE);
        }
        return;
    }

    // Started by Windows at logon the working directory is System32 —
    // make it the install folder instead.
    set_working_directory();

    let _boot_session = match boot::user_session() {
        Ok(session) => session,
        Err(e) => {
            log_error!("Display handover: {e}");
            return;
        }
    };

    log_info!("{}", "=".repeat(60));
    log_info!(
        "DS916 Tray {} starting{} (log level: {})",
        env!("CARGO_PKG_VERSION"),
        if at_logon { " at Windows sign-in" } else { "" },
        cfg.log_level
    );

    app::init(cfg.clone());

    // LibreHardwareMonitor at startup. If it isn't reachable yet (not started,
    // or web server off) that's not fatal: reads keep retrying in the
    // background, and discovery runs as soon as it connects.
    let reachable = lhm::refresh(&cfg.lhm_host_or_default(), cfg.lhm_port).is_some();
    if reachable {
        app::run_discovery();
    } else {
        log_info!("LibreHardwareMonitor: unavailable for now - will keep retrying on each sensor read");
        log_info!("  (run LHM as administrator with Options -> Remote Web Server -> Run enabled)");
    }

    autostart::clear_run_as_admin_flag();
    // (Re)register the startup entry every launch so it always points at this exe
    if cfg.autostart {
        autostart::set_autostart(true);
    }

    // Auto-load the last theme and start streaming. The display thread waits
    // for the DS916 to show up (it can take a while after a cold boot) instead
    // of giving up on the first failed port open.
    app::start_saved_display();

    app::start_lhm_poller();
    tray::run();

    app::stop_display();
    log_info!("DS916 Tray exited");
}

fn set_working_directory() {
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let _ = std::env::set_current_dir(dir);
    }
}

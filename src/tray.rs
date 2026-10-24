//! System tray icon + menu, implemented directly on Win32 (no GUI toolkit in
//! the background process — that's what keeps it small).
//!
//! Robust against the logon race: when the app is started by the Run key at
//! sign-in, Explorer's notification area may not exist yet, so adding the icon
//! is retried on a timer, and it is re-added whenever Explorer (re)creates the
//! taskbar ("TaskbarCreated").

use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, KillTimer, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, TrackPopupMenu,
    TranslateMessage, HICON, ICONINFO, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_NULL,
    WM_QUERYENDSESSION, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};

use crate::app::{self, LINK_STREAMING, LINK_WAITING};
use crate::winutil::{self, is_null, wide, zero, Icon};
use crate::{autostart, paths};

const WM_TRAY: u32 = WM_APP + 1;
const TIMER_ADD_ICON: usize = 1;

const ID_TOGGLE: usize = 100;
const ID_LOAD: usize = 101;
const ID_BUILDER: usize = 102;
const ID_DISCOVER: usize = 103;
const ID_STATUS: usize = 104;
const ID_SETTINGS: usize = 105;
const ID_UNINSTALL: usize = 106;
const ID_EXIT: usize = 107;

static mut TASKBAR_CREATED: u32 = 0;
static mut ICON: usize = 0;
static mut ICON_ADDED: bool = false;

fn icon() -> HICON {
    unsafe { winutil::usize_to_hwnd(ICON) as HICON }
}

/// The DS916 icon (same design as before), drawn at the system's small-icon size.
fn make_icon() -> HICON {
    use tiny_skia::*;
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
    let mut pm = match Pixmap::new(size, size) {
        Some(p) => p,
        None => return zero(),
    };
    let t = Transform::from_scale(size as f32 / 64.0, size as f32 / 64.0);
    let mut p = Paint::default();
    p.anti_alias = true;
    if let Some(c) = PathBuilder::from_circle(32.0, 32.0, 28.0) {
        p.set_color_rgba8(0, 20, 30, 255);
        pm.fill_path(&c, &p, FillRule::Winding, t, None);
    }
    if let Some(c) = PathBuilder::from_circle(32.0, 32.0, 26.5) {
        p.set_color_rgba8(0, 180, 255, 255);
        pm.stroke_path(&c, &p, &Stroke { width: 3.0, ..Stroke::default() }, t, None);
    }
    let mut rect = |l: f32, tp: f32, r: f32, b: f32, c: [u8; 4]| {
        p.set_color_rgba8(c[0], c[1], c[2], c[3]);
        if let Some(rc) = Rect::from_ltrb(l, tp, r, b) {
            pm.fill_rect(rc, &p, t, None);
        }
    };
    rect(20.0, 16.0, 45.0, 49.0, [0, 180, 255, 200]);
    rect(22.0, 18.0, 43.0, 47.0, [0, 10, 20, 255]);
    for i in 0..3 {
        let y0 = 22.0 + i as f32 * 8.0;
        rect(25.0, y0, 40.0, y0 + 6.0, [0, 180, 255, 180]);
    }

    unsafe {
        let mut bmi: BITMAPINFO = zero();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(zero(), &bmi, DIB_RGB_COLORS, &mut bits, zero(), 0);
        if is_null(color) || bits.is_null() {
            return zero();
        }
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
        for (i, px) in pm.pixels().iter().enumerate() {
            let c = px.demultiply();
            dst[i * 4] = c.blue();
            dst[i * 4 + 1] = c.green();
            dst[i * 4 + 2] = c.red();
            dst[i * 4 + 3] = c.alpha();
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, std::ptr::null());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let h = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        h
    }
}

fn tooltip() -> String {
    let status = if !app::is_running() {
        "○ Stopped"
    } else if app::app().link.load(Ordering::Relaxed) == LINK_STREAMING {
        "● Running"
    } else {
        "◌ Waiting for DS916…"
    };
    let theme = app::theme_name().unwrap_or_else(|| "No theme loaded".into());
    format!("DS916 — {status}\n{theme}")
}

fn notify_data(hwnd: HWND, flags: u32) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = zero();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = flags;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon();
    let tip: Vec<u16> = tooltip().encode_utf16().take(127).collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid
}

fn add_icon(hwnd: HWND) -> bool {
    let nid = notify_data(hwnd, NIF_ICON | NIF_MESSAGE | NIF_TIP);
    unsafe {
        if Shell_NotifyIconW(NIM_ADD, &nid) != 0 {
            ICON_ADDED = true;
            return true;
        }
        // Already there (e.g. Explorer kept it) — a modify succeeding is good enough
        if Shell_NotifyIconW(NIM_MODIFY, &nid) != 0 {
            ICON_ADDED = true;
            return true;
        }
    }
    false
}

fn update_tip(hwnd: HWND) {
    unsafe {
        if !ICON_ADDED {
            return;
        }
        let nid = notify_data(hwnd, NIF_TIP);
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn remove_icon(hwnd: HWND) {
    unsafe {
        let nid = notify_data(hwnd, 0);
        Shell_NotifyIconW(NIM_DELETE, &nid);
        ICON_ADDED = false;
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let m = CreatePopupMenu();
        if is_null(m) {
            return;
        }
        let add = |id: usize, text: &str, flags: u32| {
            let w = wide(text);
            AppendMenuW(m, flags, id, w.as_ptr());
        };
        let sep = || {
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
        };
        add(0, "DS916 Screen Manager", MF_STRING | MF_GRAYED);
        sep();
        add(ID_TOGGLE, if app::is_running() { "⏹ Stop Display" } else { "▶ Start Display" }, MF_STRING);
        add(ID_LOAD, "📂 Load Theme…", MF_STRING);
        add(ID_BUILDER, "🎨 Open Theme Builder", MF_STRING);
        sep();
        add(ID_DISCOVER, "🔍 Discover Sensors", MF_STRING);
        add(ID_STATUS, "ℹ Status…", MF_STRING);
        add(ID_SETTINGS, "⚙ Settings…", MF_STRING);
        sep();
        add(ID_UNINSTALL, "🗑 Uninstall…", MF_STRING);
        add(ID_EXIT, "❌ Exit", MF_STRING);

        let mut pt: POINT = zero();
        GetCursorPos(&mut pt);
        // Required so the menu closes when clicking elsewhere
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, std::ptr::null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(m);
        if cmd > 0 {
            on_command(hwnd, cmd as usize);
        }
    }
}

/// PID of the running tray app (owner of the tray message window)
fn tray_pid() -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let h = winutil::find_window(Some(winutil::TRAY_CLASS), None)?;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    if pid == 0 { None } else { Some(pid) }
}

/// Launch the Settings/Status window in its own short-lived process (so the GUI's
/// memory is released as soon as it's closed). If it's already open, focus it.
pub fn spawn_window(arg: &str, title: &str) {
    if let Some(h) = winutil::find_window(None, Some(title)) {
        winutil::bring_to_front(h);
        return;
    }
    match std::env::current_exe() {
        Ok(exe) => {
            let mut cmd = std::process::Command::new(exe);
            cmd.arg(arg);
            // Tie the window's lifetime to the running tray process
            if let Some(pid) = tray_pid() {
                cmd.arg("--parent").arg(pid.to_string());
            }
            if let Err(e) = cmd.spawn() {
                crate::log_error!("Could not open {title}: {e}");
            }
        }
        Err(e) => crate::log_error!("Could not open {title}: {e}"),
    }
}

fn on_command(hwnd: HWND, id: usize) {
    match id {
        ID_TOGGLE => {
            if app::is_running() {
                app::stop_display();
            } else {
                app::start_display();
            }
        }
        ID_LOAD => {
            let picked = rfd::FileDialog::new()
                .set_title("Load DS916 Theme")
                .add_filter("DS916 Theme", &["zip", "ds916theme"])
                .add_filter("All files", &["*"])
                .pick_file();
            if let Some(p) = picked {
                let path = p.display().to_string();
                if app::load_theme(&path) {
                    if !app::is_running() {
                        app::start_display();
                    }
                    crate::log_info!("Theme loaded and display started: {path}");
                } else {
                    winutil::message_box("DS916", "Could not load that theme — see the log for details.", Icon::Warn);
                }
            }
        }
        ID_BUILDER => {
            let html = paths::theme_builder_file();
            if html.exists() {
                winutil::shell_open(&html.display().to_string());
            } else {
                winutil::message_box(
                    "DS916",
                    &format!(
                        "theme_builder.html not found in:\n{}\n\nIf you built this app yourself, re-run build.bat to reinstall it there.",
                        paths::config_dir().display()
                    ),
                    Icon::Warn,
                );
            }
        }
        ID_DISCOVER => match app::run_discovery() {
            Some((path, n)) => winutil::message_box(
                "DS916 — Sensor Discovery",
                &format!(
                    "✅ {n} sensors discovered and saved.\n\n{}\n\nOpen the Theme Builder and click \"Import Sensor List\" to use them.",
                    path.display()
                ),
                Icon::Info,
            ),
            None => winutil::message_box(
                "DS916 — Sensor Discovery",
                "Could not discover sensors.\nMake sure LibreHardwareMonitor is running (as administrator) with\n\
                 Options → Remote Web Server → Run enabled.",
                Icon::Warn,
            ),
        },
        ID_STATUS => spawn_window("--status", winutil::STATUS_TITLE),
        ID_SETTINGS => spawn_window("--settings", winutil::SETTINGS_TITLE),
        ID_UNINSTALL => uninstall(hwnd),
        ID_EXIT => quit(hwnd),
        _ => {}
    }
    update_tip(hwnd);
}

fn quit(hwnd: HWND) {
    app::stop_display();
    remove_icon(hwnd);
    unsafe {
        DestroyWindow(hwnd);
    }
}

fn uninstall(hwnd: HWND) {
    let ok = winutil::ask_yes_no(
        "DS916 Tray — Uninstall",
        "This will:\n\n  • Remove DS916Tray from Windows startup\n  • Remove the Desktop and Start Menu shortcuts\n  \
         • Delete theme_builder.html, config, and discovered sensor data\n  • Close the tray app\n\n\
         Your theme files (.ds916theme) will NOT be deleted.\n\n\
         Note: DS916Tray.exe itself can't delete itself while running — it will be left behind in the (now \
         otherwise empty) install folder. You can delete it manually once the app has closed.\n\nContinue?",
    );
    if !ok {
        return;
    }
    // Remove boot startup before deleting its config or helper script.
    if !remove_boot_task() {
        winutil::message_box(
            "DS916 Tray — Uninstall paused",
            "Could not remove the boot startup task. No app data was deleted.\n\nRun enable-boot-start.ps1 -Action Remove in an administrator PowerShell using your normal account, then retry Uninstall.",
            Icon::Error,
        );
        return;
    }
    // 1. Remove from startup
    autostart::set_autostart(false);
    // 2. Desktop and Start Menu shortcuts created by build.bat
    for d in [winutil::desktop_dir(), winutil::start_menu_programs_dir()].into_iter().flatten() {
        let lnk = d.join("DS916 Tray.lnk");
        if lnk.exists() {
            match std::fs::remove_file(&lnk) {
                Ok(()) => crate::log_info!("Removed shortcut: {}", lnk.display()),
                Err(e) => crate::log_warn!("Shortcut removal error: {e}"),
            }
        }
    }
    // 3. Everything in the config dir except the running exe and Themes\
    app::stop_display();
    let dir = paths::config_dir();
    let exe_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "ds916tray.exe".into());
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name == exe_name || name == "ds916tray.exe" || name == "themes" {
                continue;
            }
            let p = e.path();
            let r = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            match r {
                Ok(()) => crate::log_info!("Removed: {}", p.display()),
                Err(e) => crate::log_warn!("Removal error for {}: {e}", p.display()),
            }
        }
    }
    winutil::message_box(
        "DS916 Tray — Uninstalled",
        &format!(
            "DS916 Tray has been removed from startup, and shortcuts and data have been deleted.\n\n\
             DS916Tray.exe itself is left in:\n{}\n\nYou can delete that file manually now that the app is closing.",
            dir.display()
        ),
        Icon::Info,
    );
    quit(hwnd);
}

fn remove_boot_task() -> bool {
    use std::os::windows::process::CommandExt;
    let script = paths::config_dir().join("enable-boot-start.ps1");
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let powershell = std::path::PathBuf::from(system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    match std::process::Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(["-Action", "Remove"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .status()
    {
        Ok(status) if status.success() => true,
        other => {
            crate::log_error!("Could not remove boot startup task: {other:?}");
            false
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TRAY {
        match lparam as u32 {
            WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
            WM_LBUTTONDBLCLK => spawn_window("--status", winutil::STATUS_TITLE),
            _ => {}
        }
        return 0;
    }
    if TASKBAR_CREATED != 0 && msg == TASKBAR_CREATED {
        // Explorer restarted (or started late at logon): put the icon back
        ICON_ADDED = false;
        if !add_icon(hwnd) {
            SetTimer(hwnd, TIMER_ADD_ICON, 2000, None);
        }
        return 0;
    }
    match msg {
        WM_TIMER if wparam == TIMER_ADD_ICON => {
            if add_icon(hwnd) {
                KillTimer(hwnd, TIMER_ADD_ICON);
                crate::log_info!("Tray icon added");
            }
            0
        }
        winutil::WM_APP_STATE_CHANGED => {
            update_tip(hwnd);
            0
        }
        winutil::WM_APP_RELOAD_CONFIG => {
            app::reload_config();
            update_tip(hwnd);
            0
        }
        winutil::WM_APP_QUERY_STATE => {
            // 0 = stopped, 1 = waiting for the DS916, 2 = streaming
            if !app::is_running() {
                0
            } else {
                app::app().link.load(Ordering::Relaxed).max(LINK_WAITING) as LRESULT
            }
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                crate::log_info!("Windows is signing out / shutting down - stopping display");
                app::stop_display();
                remove_icon(hwnd);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Create the hidden window + tray icon and run the message loop until Exit.
pub fn run() {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide(winutil::TRAY_CLASS);
        let mut wc: WNDCLASSW = zero();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        let title = wide("DS916 Tray");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            zero(),
            zero(),
            hinst,
            std::ptr::null(),
        );
        if is_null(hwnd) {
            crate::log_error!("Could not create the tray window");
            return;
        }
        app::app().hwnd.store(winutil::hwnd_to_usize(hwnd), Ordering::Relaxed);
        let tc = wide("TaskbarCreated");
        TASKBAR_CREATED = RegisterWindowMessageW(tc.as_ptr());
        ICON = winutil::hwnd_to_usize(make_icon() as HWND);

        if !add_icon(hwnd) {
            crate::log_info!("Notification area not ready yet (normal right after sign-in) - retrying");
            SetTimer(hwnd, TIMER_ADD_ICON, 2000, None);
        }

        let mut msg: MSG = zero();
        while GetMessageW(&mut msg, zero(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        app::app().hwnd.store(0, Ordering::Relaxed);
        if ICON != 0 {
            DestroyIcon(icon());
        }
    }
}

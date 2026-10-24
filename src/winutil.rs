//! Small Win32 helpers shared by the tray and the settings/status windows.

use std::path::PathBuf;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::UI::Shell::{SHGetFolderPathW, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, MessageBoxW, SetForegroundWindow, ShowWindow, IDYES, MB_ICONERROR, MB_ICONINFORMATION,
    MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, SW_RESTORE, SW_SHOWNORMAL,
};

/// Window class of the tray's hidden message window (used by the settings /
/// status processes to talk to the running tray app).
pub const TRAY_CLASS: &str = "DS916TrayMessageWindow";
pub const SETTINGS_TITLE: &str = "DS916 Settings";
pub const STATUS_TITLE: &str = "DS916 Status";

/// WM_APP messages understood by the tray window
pub const WM_APP_RELOAD_CONFIG: u32 = 0x8000 + 10;
pub const WM_APP_QUERY_STATE: u32 = 0x8000 + 11;
pub const WM_APP_STATE_CHANGED: u32 = 0x8000 + 12;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// All-zero value of a Win32 handle/struct type (works whether windows-sys
/// models handles as pointers or integers).
pub fn zero<T>() -> T {
    unsafe { std::mem::zeroed() }
}

pub fn is_null<T: Copy>(h: T) -> bool {
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<usize>());
    unsafe { std::mem::transmute_copy::<T, usize>(&h) == 0 }
}

pub fn hwnd_to_usize(h: HWND) -> usize {
    unsafe { std::mem::transmute_copy::<HWND, usize>(&h) }
}

pub fn usize_to_hwnd(v: usize) -> HWND {
    unsafe { std::mem::transmute_copy::<usize, HWND>(&v) }
}

#[derive(Clone, Copy)]
pub enum Icon {
    Info,
    Warn,
    Error,
}

pub fn message_box(title: &str, text: &str, icon: Icon) {
    let t = wide(title);
    let m = wide(text);
    let i = match icon {
        Icon::Info => MB_ICONINFORMATION,
        Icon::Warn => MB_ICONWARNING,
        Icon::Error => MB_ICONERROR,
    };
    unsafe {
        MessageBoxW(zero(), m.as_ptr(), t.as_ptr(), MB_OK | i | MB_SETFOREGROUND | MB_TOPMOST);
    }
}

pub fn ask_yes_no(title: &str, text: &str) -> bool {
    let t = wide(title);
    let m = wide(text);
    unsafe {
        MessageBoxW(zero(), m.as_ptr(), t.as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST)
            == IDYES
    }
}

/// Open a file, folder or URL with its default handler.
pub fn shell_open(target: &str) -> bool {
    let op = wide("open");
    let t = wide(target);
    let r = unsafe { ShellExecuteW(zero(), op.as_ptr(), t.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    // ShellExecute returns a value > 32 on success
    (unsafe { std::mem::transmute_copy::<_, usize>(&r) }) > 32
}

const CSIDL_PROGRAMS: i32 = 0x02;
const CSIDL_DESKTOPDIRECTORY: i32 = 0x10;

fn folder(csidl: i32) -> Option<PathBuf> {
    let mut buf = [0u16; 260];
    let hr = unsafe { SHGetFolderPathW(zero(), csidl, zero(), 0, buf.as_mut_ptr()) };
    if hr != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

pub fn desktop_dir() -> Option<PathBuf> {
    folder(CSIDL_DESKTOPDIRECTORY)
}

pub fn start_menu_programs_dir() -> Option<PathBuf> {
    folder(CSIDL_PROGRAMS)
}

/// Find a top-level window by class and/or title.
pub fn find_window(class: Option<&str>, title: Option<&str>) -> Option<HWND> {
    let c = class.map(wide);
    let t = title.map(wide);
    let h = unsafe {
        FindWindowW(
            c.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null()),
            t.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null()),
        )
    };
    if is_null(h) { None } else { Some(h) }
}

pub fn bring_to_front(h: HWND) {
    unsafe {
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetForegroundWindow(h);
    }
}

/// (total, used, free) bytes for a drive root like "C:\\"
pub fn disk_usage(root: &str) -> Option<(u64, u64, u64)> {
    let w = wide(root);
    let mut avail = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 || total == 0 {
        return None;
    }
    Some((total, total.saturating_sub(free), free))
}

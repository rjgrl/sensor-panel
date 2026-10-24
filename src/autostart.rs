//! "Start with Windows".
//!
//! Registered in HKCU\Software\Microsoft\Windows\CurrentVersion\Run as
//!   "C:\...\DS916Tray.exe" --autostart
//!
//! Windows can silently keep a Run entry from launching: Task Manager's
//! Startup tab (and Settings -> Apps -> Startup) store an enabled/disabled flag
//! per entry under ...\Explorer\StartupApproved\Run. If that flag says
//! "disabled", the Run value is ignored at logon even though it exists. When
//! the app's own "Start with Windows" setting is on, that flag is reset to
//! "enabled" so the two never disagree.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE, REG_SZ,
};

use crate::paths::APP_NAME;
use crate::winutil::{wide, zero};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const LAYERS_KEY: &str = r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers";
const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
pub const AUTOSTART_ARG: &str = "--autostart";

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn open(path: &str, access: u32) -> Option<Key> {
    let p = wide(path);
    let mut h: HKEY = zero();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, p.as_ptr(), 0, access, &mut h) };
    if r == ERROR_SUCCESS { Some(Key(h)) } else { None }
}

fn create(path: &str) -> Option<Key> {
    let p = wide(path);
    let mut h: HKEY = zero();
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            p.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            std::ptr::null(),
            &mut h,
            std::ptr::null_mut(),
        )
    };
    if r == ERROR_SUCCESS { Some(Key(h)) } else { None }
}

fn query(k: &Key, name: &str) -> Option<Vec<u8>> {
    let n = wide(name);
    let mut size: u32 = 0;
    let mut ty: u32 = 0;
    unsafe {
        if RegQueryValueExW(k.0, n.as_ptr(), std::ptr::null(), &mut ty, std::ptr::null_mut(), &mut size) != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if RegQueryValueExW(k.0, n.as_ptr(), std::ptr::null(), &mut ty, buf.as_mut_ptr(), &mut size) != ERROR_SUCCESS {
            return None;
        }
        buf.truncate(size as usize);
        Some(buf)
    }
}

fn query_string(k: &Key, name: &str) -> Option<String> {
    let b = query(k, name)?;
    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    Some(String::from_utf16_lossy(&w[..end]))
}

fn set_string(k: &Key, name: &str, value: &str) -> bool {
    let n = wide(name);
    let v = wide(value);
    unsafe {
        RegSetValueExW(k.0, n.as_ptr(), 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32) == ERROR_SUCCESS
    }
}

fn set_binary(k: &Key, name: &str, value: &[u8]) -> bool {
    let n = wide(name);
    unsafe { RegSetValueExW(k.0, n.as_ptr(), 0, REG_BINARY, value.as_ptr(), value.len() as u32) == ERROR_SUCCESS }
}

fn delete(k: &Key, name: &str) {
    let n = wide(name);
    unsafe {
        RegDeleteValueW(k.0, n.as_ptr());
    }
}

pub fn command_line() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("\"{}\" {}", exe.display(), AUTOSTART_ARG)
}

/// Odd first byte = disabled in Task Manager / Settings -> Startup
fn approved_disabled() -> bool {
    open(APPROVED_KEY, KEY_QUERY_VALUE)
        .and_then(|k| query(&k, APP_NAME))
        .map(|b| b.first().map(|x| x & 1 == 1).unwrap_or(false))
        .unwrap_or(false)
}

pub fn set_autostart(enable: bool) {
    if enable {
        let cmd = command_line();
        match create(RUN_KEY) {
            Some(k) => {
                if query_string(&k, APP_NAME).as_deref() != Some(cmd.as_str()) {
                    if set_string(&k, APP_NAME, &cmd) {
                        crate::log_info!("Autostart registered: {cmd}");
                    } else {
                        crate::log_error!("Autostart error: could not write the Run registry value");
                    }
                }
            }
            None => crate::log_error!("Autostart error: could not open HKCU\\{RUN_KEY}"),
        }
        if approved_disabled() {
            if let Some(k) = create(APPROVED_KEY) {
                // 0x02 followed by 11 zero bytes = "enabled"
                let mut v = [0u8; 12];
                v[0] = 0x02;
                if set_binary(&k, APP_NAME, &v) {
                    crate::log_warn!(
                        "Windows had DS916Tray disabled as a startup app (Task Manager -> Startup); re-enabled it \
                         because \"Start display automatically with Windows\" is on"
                    );
                }
            }
        }
    } else {
        if let Some(k) = open(RUN_KEY, KEY_SET_VALUE) {
            delete(&k, APP_NAME);
        }
        if let Some(k) = open(APPROVED_KEY, KEY_SET_VALUE) {
            delete(&k, APP_NAME);
        }
    }
}

/// True only if the Run entry exists, points at this exe and isn't disabled by Windows.
pub fn is_autostart() -> bool {
    let registered = open(RUN_KEY, KEY_QUERY_VALUE).and_then(|k| query_string(&k, APP_NAME));
    match registered {
        Some(v) => {
            let exe = std::env::current_exe().map(|p| p.display().to_string().to_lowercase()).unwrap_or_default();
            v.to_lowercase().contains(&exe) && !approved_disabled() && !has_run_as_admin_flag()
        }
        None => false,
    }
}

/// Windows silently skips Run-key startup entries that need elevation at
/// sign-in. A leftover "Run this program as an administrator" compatibility
/// flag (RUNASADMIN) on DS916Tray.exe therefore stops it from ever starting
/// with Windows. The app needs no admin rights (COM port, LHM's web server and
/// RTSS all work without them), so remove that one flag from its own entry and
/// keep any other compatibility settings.
pub fn clear_run_as_admin_flag() {
    let exe = match std::env::current_exe() {
        Ok(p) => p.display().to_string(),
        Err(_) => return,
    };
    let k = match open(LAYERS_KEY, KEY_QUERY_VALUE | KEY_SET_VALUE) {
        Some(k) => k,
        None => return,
    };
    let current = match query_string(&k, &exe) {
        Some(v) => v,
        None => return,
    };
    if !current.split_whitespace().any(|t| t.eq_ignore_ascii_case("RUNASADMIN")) {
        return;
    }
    let rest: Vec<&str> = current.split_whitespace().filter(|t| !t.eq_ignore_ascii_case("RUNASADMIN")).collect();
    let only_marker = rest.iter().all(|t| *t == "~" || *t == "$");
    if only_marker {
        delete(&k, &exe);
    } else {
        set_string(&k, &exe, &rest.join(" "));
    }
    crate::log_warn!(
        "Removed the \"Run as administrator\" compatibility flag from {exe} - Windows skips startup apps that need \
         elevation at sign-in, which kept DS916 Tray from starting with Windows. (Takes effect from the next launch.)"
    );
}

fn has_run_as_admin_flag() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p.display().to_string(),
        Err(_) => return false,
    };
    open(LAYERS_KEY, KEY_QUERY_VALUE)
        .and_then(|k| query_string(&k, &exe))
        .map(|v| v.split_whitespace().any(|t| t.eq_ignore_ascii_case("RUNASADMIN")))
        .unwrap_or(false)
}

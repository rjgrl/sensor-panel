//! Pre-sign-in display and handover to the same user's interactive tray.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, HANDLE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::{app, config::Config, winutil::wide};

const STOP: &str = "Global\\DS916BootStop";
const DONE: &str = "Global\\DS916BootDone";

struct LocalMemory(*mut std::ffi::c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::LocalFree(self.0);
        }
    }
}

fn event_security() -> Result<LocalMemory, String> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!(
            "Cannot read boot event owner: {}",
            std::io::Error::last_os_error()
        ));
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut size = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut size,
        );
    }
    if size == 0 {
        return Err(format!(
            "Cannot size boot event owner: {}",
            std::io::Error::last_os_error()
        ));
    }
    // TOKEN_USER contains a pointer and must be aligned, including its SID buffer.
    let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(format!(
            "Cannot read boot event owner: {}",
            std::io::Error::last_os_error()
        ));
    }
    let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(format!(
            "Cannot format boot event owner: {}",
            std::io::Error::last_os_error()
        ));
    }
    let _text_memory = LocalMemory(text.cast());
    let mut len = 0;
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
    }
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    // S4U defaults can exclude the interactive token. Grant only this user and SYSTEM;
    // label medium so a normal tray can signal events created by an elevated worker.
    let sddl = wide(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})S:(ML;;NW;;;ME)"));
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!(
            "Cannot secure boot event: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(LocalMemory(descriptor))
}

struct Event(HANDLE);

// Each handle has one owner; Windows events can be waited on from any thread.
unsafe impl Send for Event {}

impl Event {
    fn create(name: &str) -> Result<(Self, bool), String> {
        let security = event_security()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: 0,
        };
        let handle = unsafe { CreateEventW(&attributes, 1, 0, wide(name).as_ptr()) };
        let error = unsafe { GetLastError() };
        if handle.is_null() {
            return Err(format!("Cannot create boot event (Windows error {error})"));
        }
        Ok((Self(handle), error == ERROR_ALREADY_EXISTS))
    }

    fn signal(&self) -> Result<(), String> {
        if unsafe { SetEvent(self.0) } == 0 {
            return Err(format!(
                "Cannot signal boot event: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn wait(&self, timeout_ms: u32) -> Result<bool, String> {
        match unsafe { WaitForSingleObject(self.0, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(format!(
                "Cannot wait for boot event: {}",
                std::io::Error::last_os_error()
            )),
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct BootSession {
    stop: Event,
    done: Event,
}

impl BootSession {
    fn new(stop_name: &str, done_name: &str) -> Result<Option<Self>, String> {
        let (stop, exists) = Event::create(stop_name)?;
        if exists {
            return Ok(None);
        }
        let (done, exists) = Event::create(done_name)?;
        if exists {
            return Err("Previous boot handover has not finished".into());
        }
        Ok(Some(Self { stop, done }))
    }
}

impl Drop for BootSession {
    fn drop(&mut self) {
        let _ = self.done.signal();
    }
}

/// Retain this guard until tray exit so a delayed boot task cannot take over.
pub struct UserSession {
    _stop: Event,
}

fn user_session_named(
    stop_name: &str,
    done_name: &str,
    timeout_ms: u32,
) -> Result<UserSession, String> {
    let (stop, _) = Event::create(stop_name)?;
    stop.signal()?;
    let handle = unsafe { OpenEventW(0x00100000, 0, wide(done_name).as_ptr()) }; // SYNCHRONIZE
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error != ERROR_FILE_NOT_FOUND {
            return Err(format!(
                "Cannot open boot completion event (Windows error {error})"
            ));
        }
    } else if !Event(handle).wait(timeout_ms)? {
        return Err(
            "Boot display did not release the screen within 15 seconds; try starting the app again"
                .into(),
        );
    }
    Ok(UserSession { _stop: stop })
}

pub fn user_session() -> Result<UserSession, String> {
    user_session_named(STOP, DONE, 15_000)
}

pub fn run(cfg: Config) -> Result<(), String> {
    let Some(session) = BootSession::new(STOP, DONE)? else {
        crate::log_info!("Boot display skipped: another display owner is already running");
        return Ok(());
    };
    if session.stop.wait(0)? {
        return Ok(());
    }
    crate::log_info!(
        "DS916 boot display starting (saved theme: {})",
        cfg.theme_path
    );
    app::init(cfg);
    if !app::start_saved_display() {
        return Err(
            "Could not start the saved theme; select a valid local theme in Settings".into(),
        );
    }
    app::start_lhm_poller();
    let result = loop {
        match session.stop.wait(1000) {
            Ok(true) => break Ok(()),
            Ok(false) if app::is_running() => {}
            Ok(false) => break Err("Boot display stopped unexpectedly".into()),
            Err(e) => break Err(e),
        }
    };
    // Join the display thread and close COM before acknowledging the handover.
    app::stop_display();
    crate::log_info!("Boot display released the screen");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn names(test: &str) -> (String, String) {
        let base = format!("Global\\DS916Test{}{}", std::process::id(), test);
        (format!("{base}Stop"), format!("{base}Done"))
    }

    #[test]
    fn user_waits_for_boot_release_and_blocks_late_boot() {
        let (stop, done) = names("Handover");
        let boot = BootSession::new(&stop, &done).unwrap().unwrap();
        assert!(BootSession::new(&stop, &done).unwrap().is_none());
        let released = Arc::new(AtomicBool::new(false));
        let worker_released = released.clone();
        let worker = std::thread::spawn(move || {
            assert!(boot.stop.wait(5000).unwrap());
            worker_released.store(true, Ordering::SeqCst);
            drop(boot);
        });
        let user = user_session_named(&stop, &done, 5000).unwrap();
        assert!(released.load(Ordering::SeqCst));
        assert!(BootSession::new(&stop, &done).unwrap().is_none());
        worker.join().unwrap();
        drop(user);
        assert!(BootSession::new(&stop, &done).unwrap().is_some());
    }

    #[test]
    fn user_starting_first_blocks_boot() {
        let (stop, done) = names("UserFirst");
        let user = user_session_named(&stop, &done, 100).unwrap();
        assert!(BootSession::new(&stop, &done).unwrap().is_none());
        drop(user);
        assert!(BootSession::new(&stop, &done).unwrap().is_some());
    }

    #[test]
    fn timeout_does_not_start_another_display_owner() {
        let (stop, done) = names("Timeout");
        let boot = BootSession::new(&stop, &done).unwrap().unwrap();
        assert!(user_session_named(&stop, &done, 10).is_err());
        assert!(boot.stop.wait(0).unwrap());
        drop(boot);
    }

    #[test]
    fn control_event_access_does_not_depend_on_logon_default_dacl() {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
        use windows_sys::Win32::Security::{
            GetSecurityDescriptorDacl, ImpersonateSelf, RevertToSelf, SecurityImpersonation,
            SetTokenInformation, TokenDefaultDacl, TOKEN_ADJUST_DEFAULT, TOKEN_DEFAULT_DACL,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

        struct Restore(HANDLE, *mut std::ffi::c_void);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    RevertToSelf();
                    if !self.0.is_null() {
                        CloseHandle(self.0);
                    }
                    if !self.1.is_null() {
                        LocalFree(self.1);
                    }
                }
            }
        }
        let mut restore = Restore(std::ptr::null_mut(), std::ptr::null_mut());
        unsafe {
            assert_ne!(ImpersonateSelf(SecurityImpersonation), 0);
            assert_ne!(
                OpenThreadToken(GetCurrentThread(), TOKEN_ADJUST_DEFAULT, 1, &mut restore.0),
                0
            );
            // Simulate a noninteractive logon's DACL that excludes the tray's token.
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide("D:(A;;GA;;;SY)").as_ptr(),
                    1,
                    &mut restore.1,
                    std::ptr::null_mut()
                ),
                0
            );
            let mut acl = std::ptr::null_mut();
            let mut present = 0;
            let mut defaulted = 0;
            assert_ne!(
                GetSecurityDescriptorDacl(restore.1, &mut present, &mut acl, &mut defaulted),
                0
            );
            let dacl = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
            assert_ne!(
                SetTokenInformation(
                    restore.0,
                    TokenDefaultDacl,
                    &dacl as *const _ as *const _,
                    std::mem::size_of_val(&dacl) as u32
                ),
                0
            );
        }
        let (name, _) = names("RestrictedDacl");
        let (_event, existed) = Event::create(&name).unwrap();
        assert!(!existed);
        let handle = unsafe { OpenEventW(0x00100002, 0, wide(&name).as_ptr()) };
        assert!(
            !handle.is_null(),
            "same user must be able to signal and wait regardless of the creator's logon DACL"
        );
        let reopened = Event(handle);
        reopened.signal().unwrap();
        assert!(reopened.wait(0).unwrap());
    }
}

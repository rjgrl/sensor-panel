//! RTSS reader (optional — FPS via RivaTuner Statistics Server shared memory).
//!
//! Independent of LibreHardwareMonitor. RTSS hooks the game's present calls, so
//! its per-process framerate is attributed correctly. RTSS may not be running,
//! may start later or may close at any time: the mapping is re-verified on
//! every read and re-opened when needed.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_ALL_ACCESS, FILE_MAP_READ,
    MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
};

use crate::winutil::{is_null, wide};

/// 'RTSS' signature as RTSS writes it in memory (bytes "SSTR" little-endian)
const RTSS_SIG: u32 = u32::from_le_bytes(*b"SSTR");
const RETRY: Duration = Duration::from_secs(10);
const MAX_APPS: usize = 256;

struct Mapping {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    size: usize,
}

// The mapping is only ever touched while holding the RTSS mutex.
unsafe impl Send for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}

impl Mapping {
    fn base(&self) -> *const u8 {
        self.view.Value as *const u8
    }
    fn u32_at(&self, off: usize) -> Option<u32> {
        if off.checked_add(4)? > self.size {
            return None;
        }
        Some(unsafe { std::ptr::read_unaligned(self.base().add(off) as *const u32) })
    }
    fn bytes_at(&self, off: usize, len: usize) -> Option<&[u8]> {
        if off.checked_add(len)? > self.size {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(self.base().add(off), len) })
    }
}

struct State {
    map: Option<Mapping>,
    last_attempt: Option<Instant>,
    unavailable_logged: bool,
}

static STATE: Mutex<State> = Mutex::new(State { map: None, last_attempt: None, unavailable_logged: false });

fn lock() -> std::sync::MutexGuard<'static, State> {
    match STATE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn try_open(st: &mut State) -> bool {
    st.map = None;
    let mut last_err: Option<u32> = None;
    let mut found: Option<(Mapping, &str)> = None;
    // The access mask given to OpenFileMappingW constrains MapViewOfFile on that
    // same handle, so try every (access, name) pair as a full open+map cycle.
    // Size 0 = map the whole existing section, whatever size RTSS created.
    'outer: for access in [FILE_MAP_ALL_ACCESS, FILE_MAP_READ] {
        for name in ["RTSSSharedMemoryV2", "Global\\RTSSSharedMemoryV2"] {
            let wname = wide(name);
            unsafe {
                let h = OpenFileMappingW(access, 0, wname.as_ptr());
                if is_null(h) {
                    continue;
                }
                let view = MapViewOfFile(h, access, 0, 0, 0);
                if view.Value.is_null() {
                    let e = GetLastError();
                    last_err = Some(e);
                    crate::log_debug!("  RTSS MapViewOfFile failed (error {e}) on \"{name}\" with access=0x{access:X} - trying next combination");
                    CloseHandle(h);
                    continue;
                }
                let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
                let got = VirtualQuery(view.Value, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
                let size = if got == 0 { 0 } else { mbi.RegionSize };
                found = Some((Mapping { handle: h, view, size }, name));
                break 'outer;
            }
        }
    }
    let (m, name) = match found {
        Some(f) => f,
        None => {
            if !st.unavailable_logged {
                match last_err {
                    Some(e) => crate::log_info!(
                        "RTSS shared memory could not be mapped (last error {e}) - this is optional, FPS sensor \
                         will be unavailable. Make sure RTSS (RivaTuner Statistics Server) is installed and running."
                    ),
                    None => crate::log_info!(
                        "RTSS shared memory not found (RTSS not running - this is optional, FPS sensor will be unavailable)"
                    ),
                }
                st.unavailable_logged = true;
            }
            return false;
        }
    };
    let sig = m.u32_at(0).unwrap_or(0);
    if sig != RTSS_SIG {
        crate::log_debug!("  RTSS signature mismatch: got 0x{sig:08X}, expected 0x{RTSS_SIG:08X} (0xDEAD means RTSS is shutting down)");
        return false;
    }
    let version = m.u32_at(4).unwrap_or(0);
    if version < 0x0002_0000 {
        crate::log_warn!("  RTSS version 0x{version:08X} is older than v2.0 - per-app data unavailable");
        return false;
    }
    crate::log_info!("RTSS shared memory connected OK (mapping=\"{name}\", version=0x{version:08X})");
    st.map = Some(m);
    st.unavailable_logged = false;
    true
}

struct AppEntry {
    pid: u32,
    name: String,
    time1: u32,
    fps: f64,
    off: usize,
}

/// Walk the app array. Returns None if the mapping went stale (RTSS shut down).
fn entries(m: &Mapping) -> Option<(Vec<AppEntry>, usize)> {
    if m.u32_at(0)? != RTSS_SIG {
        return None;
    }
    // v2 header: dwSignature dwVersion dwAppEntrySize dwAppArrOffset ...
    let entry_size = m.u32_at(8)? as usize;
    let arr_off = m.u32_at(12)? as usize;
    let mut out = Vec::new();
    if entry_size == 0 || entry_size > 100_000 {
        return Some((out, entry_size)); // malformed/unexpected layout
    }
    for i in 0..MAX_APPS {
        let off = arr_off + i * entry_size;
        let pid = match m.u32_at(off) {
            Some(p) => p,
            None => break,
        };
        if pid == 0 {
            continue; // empty slot
        }
        let raw = m.bytes_at(off + 4, 260).unwrap_or(&[]);
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let name = String::from_utf8_lossy(&raw[..end]).to_string();
        let time1 = m.u32_at(off + 272).unwrap_or(0);
        // dwStatFrameTimeBufFramerate (offset 5024, RTSS v2.5+) is backed by
        // RTSS's continuously-updating frametime ring buffer (units of 0.1 FPS).
        // Older RTSS: fall back to the instantaneous dwFrameTime (µs).
        let fps = if entry_size >= 5028 {
            m.u32_at(off + 5024).unwrap_or(0) as f64 / 10.0
        } else {
            let ft = m.u32_at(off + 280).unwrap_or(0);
            if ft > 0 { 1_000_000.0 / ft as f64 } else { 0.0 }
        };
        out.push(AppEntry { pid, name, time1, fps, off });
    }
    Some((out, entry_size))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// (pid, exe name, fps) for all 3D apps RTSS currently tracks (for Settings).
pub fn list_apps() -> (bool, Vec<(u32, String, f64)>) {
    let mut st = lock();
    if st.map.is_none() && !try_open(&mut st) {
        return (false, Vec::new());
    }
    let res = st.map.as_ref().and_then(entries);
    match res {
        Some((list, _)) => (
            true,
            list.into_iter().filter(|a| !a.name.is_empty()).map(|a| (a.pid, a.name, round1(a.fps))).collect(),
        ),
        None => {
            st.map = None;
            (false, Vec::new())
        }
    }
}

/// Merge RTSS_FPS (+ MIN/AVG/MAX when available) into `data`.
/// `target`: exact exe name to pin, or empty for auto (most recently rendering app).
pub fn read_framerate(target: &str, data: &mut HashMap<String, f64>) {
    let mut st = lock();
    if st.map.is_none() {
        let now = Instant::now();
        if let Some(t) = st.last_attempt {
            if now.duration_since(t) < RETRY {
                return;
            }
        }
        st.last_attempt = Some(now);
        if !try_open(&mut st) {
            return;
        }
    }
    let res = st.map.as_ref().and_then(entries);
    let (list, entry_size) = match res {
        Some(r) => r,
        None => {
            // Signature flipped (RTSS shut down) — drop the handle so the next
            // call attempts a fresh connection.
            st.map = None;
            return;
        }
    };
    let target = target.trim().to_lowercase();
    let pick = if !target.is_empty() {
        list.iter().find(|a| a.name.to_lowercase() == target)
    } else {
        // Auto: whichever app most recently rendered a frame
        list.iter().fold(None, |best: Option<&AppEntry>, a| match best {
            Some(b) if b.time1 >= a.time1 => Some(b),
            _ => Some(a),
        })
    };
    let a = match pick {
        Some(a) => a,
        None => return,
    };
    let _ = a.pid;
    data.insert("RTSS_FPS".into(), round1(a.fps));
    if entry_size >= 316 {
        if let Some(m) = st.map.as_ref() {
            // dwStatFramerateMin @304, Avg @308, Max @312 (session-based)
            for (k, o) in [("RTSS_FPS_MIN", 304), ("RTSS_FPS_AVG", 308), ("RTSS_FPS_MAX", 312)] {
                if let Some(v) = m.u32_at(a.off + o) {
                    if v > 0 {
                        data.insert(k.into(), v as f64);
                    }
                }
            }
        }
    }
}

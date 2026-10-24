//! One read of every sensor value: LibreHardwareMonitor + system drive space + RTSS.
//! Missing keys simply mean "sensor unavailable".

use std::collections::HashMap;

use crate::config::Config;
use crate::{lhm, rtss, winutil};

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

pub fn read_all(cfg: &Config, custom_map: &[(String, String)]) -> HashMap<String, f64> {
    let mut data = HashMap::with_capacity(64);
    // Latest snapshot from the background poller (never blocks on LHM)
    let (snapshot, vram) = {
        let l = lhm::lock();
        (l.current(), l.detected_vram_gb)
    };
    if let Some(s) = snapshot {
        lhm::read_values(&s, custom_map, vram, &mut data);
    }

    // Disk space for the Windows system drive (LHM reports physical disks, not volumes)
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    if let Some((total, used, free)) = winutil::disk_usage(&format!("{drive}\\")) {
        data.insert("DISK_USED".into(), used as f64 / GB);
        data.insert("DISK_FREE".into(), free as f64 / GB);
        data.insert("DISK_USAGE".into(), used as f64 / total as f64 * 100.0);
    }

    // RTSS is optional and independent of LHM
    rtss::read_framerate(&cfg.rtss_process, &mut data);
    data
}

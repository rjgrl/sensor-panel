//! LibreHardwareMonitor reader.
//!
//! Sensor values come from LHM's built-in Remote Web Server
//! (Options -> Remote Web Server -> Run), which serves the whole sensor tree
//! as JSON at http://<host>:<port>/data.json (default port 8085).
//!
//! Every node in data.json looks like:
//!   {"id": 3, "Text": "Core (Tctl/Tdie)", "Value": "45.3 °C", "SensorId": "/amdcpu/0/temperature/2",
//!    "Type": "Temperature", "RawValue": 45.25, "HardwareId": "...", "Children": [...]}
//! Older LHM releases omit RawValue/Type/HardwareId -- everything here falls
//! back to parsing the formatted Value string and the SensorId path instead.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{http, paths};

pub const TIMEOUT: Duration = Duration::from_millis(2000); // LHM occasionally takes >1s to answer

const GB: f64 = 1024.0 * 1024.0 * 1024.0;
const MB: f64 = 1024.0 * 1024.0;

// ── data.json model ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Node {
    #[serde(rename = "Text", default)]
    text: Option<String>,
    #[serde(rename = "Value", default)]
    value: Option<Value>,
    #[serde(rename = "SensorId", default)]
    sensor_id: Option<String>,
    #[serde(rename = "Type", default)]
    typ: Option<String>,
    #[serde(rename = "RawValue", default)]
    raw: Option<Value>,
    #[serde(rename = "HardwareId", default)]
    hardware_id: Option<String>,
    #[serde(rename = "Children", default)]
    children: Option<Vec<Node>>,
    #[serde(rename = "Version", default)]
    version: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct Sensor {
    pub id: String,
    pub name: String,
    pub typ: String,
    pub hardware: Option<String>,
    pub hw_id: String,
    pub category: &'static str,
    /// value in canonical unit (see normalize)
    pub value: Option<f64>,
}

/// Hardware identifier prefix (first path segment of SensorId/HardwareId) -> category
fn hw_category(prefix: &str) -> &'static str {
    match prefix {
        "amdcpu" | "intelcpu" | "genericcpu" | "cpu" => "cpu",
        "gpu-nvidia" | "gpu-amd" | "gpu-intel" | "gpu-ati" | "nvidiagpu" | "atigpu" => "gpu",
        "gpu-intel-integrated" => "igpu",
        "ram" => "ram",
        "vram" => "vram",
        "lpc" | "motherboard" | "mainboard" | "ec" | "smbios" => "mb",
        "hdd" | "ssd" | "nvme" | "ata" | "scsi" | "usb" | "sd" | "mmc" | "raid" | "sas" | "storage" => "storage",
        "nic" => "nic",
        "battery" => "battery",
        "psu" => "psu",
        _ => "other",
    }
}

/// SensorId type segment -> LHM SensorType name (older LHM has no "Type" field)
fn type_from_id(seg: &str) -> Option<&'static str> {
    Some(match seg {
        "voltage" => "Voltage",
        "current" => "Current",
        "power" => "Power",
        "clock" => "Clock",
        "temperature" => "Temperature",
        "load" => "Load",
        "frequency" => "Frequency",
        "fan" => "Fan",
        "flow" => "Flow",
        "control" => "Control",
        "level" => "Level",
        "factor" => "Factor",
        "data" => "Data",
        "smalldata" => "SmallData",
        "throughput" => "Throughput",
        "timespan" => "TimeSpan",
        "energy" => "Energy",
        "noise" => "Noise",
        "timing" => "Timing",
        "conductivity" => "Conductivity",
        "humidity" => "Humidity",
        _ => return None,
    })
}

fn byte_unit(u: &str) -> Option<f64> {
    Some(match u {
        "B" => 1.0,
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => MB,
        "GB" | "GIB" => GB,
        "TB" | "TIB" => GB * 1024.0,
        _ => return None,
    })
}

/// '45.3 °C' -> (Some(45.3), "°C"); '1,5 GB' -> (Some(1.5), "GB").
/// Handles comma decimal separators (non-English Windows locales).
pub fn parse_formatted(text: &str) -> (Option<f64>, String) {
    let t = text.trim_start();
    let bytes: Vec<char> = t.chars().collect();
    let mut i = 0;
    let mut num = String::new();
    if i < bytes.len() && bytes[i] == '-' {
        num.push('-');
        i += 1;
    }
    let start_digits = i;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.' || bytes[i] == ',') {
        num.push(bytes[i]);
        i += 1;
    }
    if i == start_digits {
        return (None, String::new());
    }
    let unit: String = bytes[i..].iter().collect::<String>().trim().to_string();
    let cleaned = if num.contains(',') && !num.contains('.') {
        num.replace(',', ".")
    } else {
        num.replace(',', "")
    };
    (cleaned.parse::<f64>().ok(), unit)
}

/// Value in a canonical unit: Temperature -> °C, Data/SmallData -> bytes,
/// Throughput -> bytes/s, everything else -> LHM's native unit.
fn normalize(stype: &str, raw: Option<f64>, text: &str) -> Option<f64> {
    let (parsed, unit) = parse_formatted(text);
    match stype {
        "Data" | "SmallData" | "Throughput" => {
            let u = unit.to_uppercase().replace("/S", "");
            let u = u.trim();
            if let (Some(p), Some(m)) = (parsed, byte_unit(u)) {
                return Some(p * m);
            }
            raw.map(|r| {
                r * match stype {
                    "Data" => GB,
                    "SmallData" => MB,
                    _ => 1.0,
                }
            })
        }
        "Temperature" => {
            if raw.is_some() {
                return raw; // LHM's raw value is always °C
            }
            match parsed {
                Some(p) if unit.contains("°F") => Some((p - 32.0) / 1.8),
                p => p,
            }
        }
        _ => raw.or(parsed),
    }
}

fn flatten(root: &Node) -> Vec<Sensor> {
    let mut out = Vec::new();
    walk(root, None, "", None, &mut out);
    out
}

fn walk(node: &Node, hw_name: Option<&str>, hw_id: &str, group_text: Option<&str>, out: &mut Vec<Sensor>) {
    let empty: Vec<Node> = Vec::new();
    let children = node.children.as_ref().unwrap_or(&empty);
    if let Some(sid) = node.sensor_id.as_deref().filter(|s| !s.is_empty()) {
        let parts: Vec<&str> = sid.split('/').filter(|p| !p.is_empty()).collect();
        let this_hw_id: String = if !hw_id.is_empty() {
            hw_id.to_string()
        } else if parts.len() > 2 {
            format!("/{}", parts[..parts.len() - 2].join("/"))
        } else {
            String::new()
        };
        let stype: String = match node.typ.as_deref().filter(|t| !t.is_empty()) {
            Some(t) => t.to_string(),
            None => {
                let seg = if parts.len() >= 2 { parts[parts.len() - 2].to_lowercase() } else { String::new() };
                match type_from_id(&seg) {
                    Some(t) => t.to_string(),
                    None => group_text.unwrap_or("").to_string(),
                }
            }
        };
        let prefix = this_hw_id
            .trim_matches('/')
            .split('/')
            .next()
            .unwrap_or("")
            .to_lowercase();
        let raw = match &node.raw {
            Some(Value::Number(n)) => n.as_f64(),
            _ => None,
        };
        let text = match &node.value {
            Some(Value::String(s)) => s.as_str(),
            _ => "",
        };
        out.push(Sensor {
            id: sid.to_string(),
            name: node.text.as_deref().unwrap_or("").trim().to_string(),
            value: normalize(&stype, raw, text),
            typ: stype,
            hardware: hw_name.map(|s| s.to_string()),
            hw_id: this_hw_id,
            category: hw_category(&prefix),
        });
        return;
    }
    // A node is a hardware node if LHM tags it with HardwareId, or (older
    // versions) if its grandchildren are sensors. Group nodes ("Temperatures",
    // "Load", ...) sit between hardware and sensors.
    let has_hwid = node.hardware_id.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
    let is_hw = has_hwid
        || children.iter().any(|g| {
            g.children
                .as_ref()
                .map(|gc| gc.iter().any(|c| c.sensor_id.as_deref().map(|s| !s.is_empty()).unwrap_or(false)))
                .unwrap_or(false)
        });
    let text = node.text.as_deref().unwrap_or("").trim();
    if is_hw {
        let id = node.hardware_id.as_deref().unwrap_or("");
        for c in children {
            walk(c, Some(text), id, None, out);
        }
    } else {
        let gt = if !text.is_empty() { Some(text) } else { group_text };
        for c in children {
            walk(c, hw_name, hw_id, gt, out);
        }
    }
}

// ── Shared client state ─────────────────────────────────────────────────────
//
// The tray app polls LHM on its own background thread (app::lhm_poller), so a
// slow or briefly unresponsive LHM never stalls the frame loop. The frame loop
// only reads the latest snapshot via `current()`.
//
// LHM's web server occasionally stops answering for a few seconds (typically
// while another program is starting and LHM's hardware polling is busy). Those
// hiccups must not blank the panel, so the last good snapshot keeps being used
// for up to STALE_LIMIT; only a longer outage (LHM closed, web server off)
// makes the sensor values go away.

/// How long the last good snapshot stays on screen while LHM doesn't answer
pub const STALE_LIMIT: Duration = Duration::from_secs(20);
/// Outages shorter than this are normal LHM hiccups: logged at verbose level only
pub const QUIET_OUTAGE: Duration = Duration::from_secs(20);

pub struct Lhm {
    snapshot: Option<Arc<Vec<Sensor>>>,
    snapshot_time: Option<Instant>,
    pub version: Option<String>,
    /// when the current run of failed requests started (None = last request OK)
    fail_since: Option<Instant>,
    /// an outage warning was written (so the recovery gets an INFO line too)
    fail_logged: bool,
    /// Fallback VRAM capacity (GB) from the known-card table
    pub detected_vram_gb: Option<f64>,
    /// set when a connection is first established (or re-established after a
    /// logged outage); consumed by the tray to run sensor discovery once LHM
    /// becomes reachable after startup
    pub newly_connected: bool,
}

pub static LHM: Mutex<Lhm> = Mutex::new(Lhm {
    snapshot: None,
    snapshot_time: None,
    version: None,
    fail_since: None,
    fail_logged: false,
    detected_vram_gb: None,
    newly_connected: false,
});

pub fn lock() -> std::sync::MutexGuard<'static, Lhm> {
    match LHM.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Fetch one snapshot right now, without touching the shared client state.
pub fn fetch_once(host: &str, port: u16) -> Result<(Vec<Sensor>, Option<String>), String> {
    let body = http::get(host, port, "/data.json", TIMEOUT)?;
    let body = body.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&body);
    let root: Node = serde_json::from_slice(body).map_err(|e| format!("invalid data.json: {e}"))?;
    let sensors = flatten(&root);
    if sensors.is_empty() {
        return Err("data.json contained no sensors".into());
    }
    let version = match &root.version {
        Some(Value::String(s)) => Some(s.clone()),
        Some(v @ Value::Number(_)) => Some(v.to_string()),
        _ => None,
    };
    Ok((sensors, version))
}

/// Request a fresh snapshot from LHM and store it. The HTTP request runs
/// without holding the shared lock, so readers are never blocked by it.
/// Returns the new snapshot, or None if this request failed.
pub fn refresh(host: &str, port: u16) -> Option<Arc<Vec<Sensor>>> {
    let res = fetch_once(host, port);
    let url = format!("http://{host}:{port}/data.json");
    lock().apply(res, &url)
}

impl Lhm {
    fn apply(&mut self, res: Result<(Vec<Sensor>, Option<String>), String>, url: &str) -> Option<Arc<Vec<Sensor>>> {
        let now = Instant::now();
        match res {
            Ok((sensors, version)) => {
                let n = sensors.len();
                let arc = Arc::new(sensors);
                let first = self.snapshot.is_none();
                self.snapshot = Some(arc.clone());
                self.snapshot_time = Some(now);
                self.version = version;
                if first || self.fail_logged {
                    crate::log_info!(
                        "LibreHardwareMonitor connected ({url}, version {}, {n} sensors)",
                        self.version.as_deref().unwrap_or("unknown")
                    );
                    self.newly_connected = true;
                } else if let Some(t) = self.fail_since {
                    crate::log_debug!(
                        "LibreHardwareMonitor answered again after {:.1}s (last values were kept on screen meanwhile)",
                        now.duration_since(t).as_secs_f64()
                    );
                }
                self.fail_since = None;
                self.fail_logged = false;
                Some(arc)
            }
            Err(e) => {
                let since = *self.fail_since.get_or_insert(now);
                let outage = now.duration_since(since);
                if !self.fail_logged && (self.snapshot.is_none() || outage >= QUIET_OUTAGE) {
                    crate::log_warn!(
                        "LibreHardwareMonitor not reachable at {url}: {e} -- make sure LHM is running and \
                         Options -> Remote Web Server -> Run is enabled. Will keep retrying."
                    );
                    self.fail_logged = true;
                } else {
                    crate::log_debug!("LibreHardwareMonitor request failed ({e}); outage so far {:.1}s", outage.as_secs_f64());
                }
                None
            }
        }
    }

    /// Latest snapshot, as long as it isn't older than STALE_LIMIT.
    pub fn current(&self) -> Option<Arc<Vec<Sensor>>> {
        match (&self.snapshot, self.snapshot_time) {
            (Some(s), Some(t)) if t.elapsed() <= STALE_LIMIT => Some(s.clone()),
            _ => None,
        }
    }

    /// True while requests are failing (used to pick the retry interval)
    pub fn failing_for(&self) -> Option<Duration> {
        self.fail_since.map(|t| t.elapsed())
    }
}

// ── Standard sensor keys ────────────────────────────────────────────────────
// Resolved by NAME on every read (nothing index-based is ever cached).
// Each rule: (categories, sensor type, candidate names in priority order, unit)
//   unit: "" = as-is, "GB" = bytes->GB, "MB/s" = bytes/s->MB/s

type Rule = (&'static [&'static str], &'static str, &'static [&'static str], &'static str);

pub static STANDARD_SENSOR_RULES: &[(&str, &[Rule])] = &[
    ("CPU_USAGE", &[(&["cpu"], "Load", &["CPU Total"], "")]),
    ("CPU_TEMP", &[
        (&["cpu"], "Temperature", &["Core (Tctl/Tdie)", "CPU Package", "Core (Tdie)", "Core (Tctl)", "Package", "Core Average", "CPU Cores"], ""),
        (&["mb"], "Temperature", &["CPU", "CPU Package", "CPU Core"], ""),
    ]),
    ("CPU_FAN", &[(&["mb"], "Fan", &["CPU Fan", "CPU Fan #1", "CPU", "Fan #1"], "")]),
    ("CPU_FREQ", &[(&["cpu"], "Clock", &["Cores (Average)", "Cores (Average Effective)", "Core #1", "CPU Core #1"], "")]),
    ("CPU_POWER", &[(&["cpu"], "Power", &["Package", "CPU Package"], "")]),
    ("CPU_VOLTAGE", &[
        (&["cpu"], "Voltage", &["Core (SVI2 TFN)", "CPU Core", "Core VID", "CPU Cores"], ""),
        (&["mb"], "Voltage", &["Vcore", "CPU Core", "CPU Vcore"], ""),
    ]),
    ("GPU_USAGE", &[(&["gpu", "igpu"], "Load", &["GPU Core", "D3D 3D"], "")]),
    ("GPU_TEMP", &[(&["gpu", "igpu"], "Temperature", &["GPU Core", "GPU Temperature"], "")]),
    ("GPU_FAN1", &[(&["gpu"], "Fan", &["GPU Fan 1", "GPU Fan", "GPU"], "")]),
    ("GPU_FAN2", &[(&["gpu"], "Fan", &["GPU Fan 2"], "")]),
    ("GPU_FREQ", &[(&["gpu", "igpu"], "Clock", &["GPU Core"], "")]),
    // 'GPU Package' is total board/ASIC power on both NVIDIA and AMD in LHM;
    // AMD's GFX core rail ('GPU Core') is only a last resort (under-reports).
    ("GPU_POWER", &[(&["gpu", "igpu"], "Power", &["GPU Package", "GPU Total", "GPU Power", "GPU PPT", "GPU Core"], "")]),
    // D3D Dedicated is what Task Manager shows and is accurate on AMD
    ("VRAM_USED", &[
        (&["gpu"], "Data", &["D3D Dedicated Memory Used", "GPU Memory Used"], "GB"),
        (&["gpu"], "SmallData", &["D3D Dedicated Memory Used", "GPU Memory Used"], "GB"),
    ]),
    ("RAM_USAGE", &[(&["ram"], "Load", &["Memory", "Physical Memory"], "")]),
    ("RAM_USED_GB", &[(&["ram"], "Data", &["Memory Used", "Physical Memory Used", "Used Memory"], "GB")]),
    ("RAM_FREE_GB", &[(&["ram"], "Data", &["Memory Available", "Physical Memory Available", "Available Memory"], "GB")]),
    ("DISK_TEMP", &[(&["storage"], "Temperature", &["Temperature", "Composite Temperature", "Temperature 1"], "")]),
    ("DISK_READ", &[(&["storage"], "Throughput", &["Read Rate"], "MB/s")]),
    ("DISK_WRITE", &[(&["storage"], "Throughput", &["Write Rate"], "MB/s")]),
    ("MB_TEMP", &[(&["mb"], "Temperature", &["Motherboard", "System", "PCH", "Mainboard"], "")]),
    ("CHASSIS_FAN1", &[(&["mb"], "Fan", &["System Fan #1", "Chassis Fan #1", "Chassis Fan 1", "System Fan", "Chassis Fan", "Fan #2"], "")]),
    ("CHASSIS_FAN2", &[(&["mb"], "Fan", &["System Fan #2", "Chassis Fan #2", "Chassis Fan 2", "Fan #3"], "")]),
    ("CHASSIS_FAN3", &[(&["mb"], "Fan", &["System Fan #3", "Chassis Fan #3", "Chassis Fan 3", "Fan #4"], "")]),
    ("BATTERY", &[(&["battery"], "Level", &["Charge Level"], "")]),
    // NET_DOWN / NET_UP: busiest network adapter wins (see read_values).
    // DISK_USAGE / DISK_USED / DISK_FREE: Windows system drive (sensors.rs).
    // FPS: RTSS keys (rtss.rs).
];

fn convert(value: Option<f64>, unit: &str) -> Option<f64> {
    let v = value?;
    Some(match unit {
        "GB" => v / GB,
        "MB/s" => v / MB,
        _ => v,
    })
}

/// Display-unit value for a sensor picked in the Theme Builder (CUSTOM_* keys)
pub fn custom_value(s: &Sensor) -> Option<f64> {
    match s.typ.as_str() {
        "Data" | "SmallData" => convert(s.value, "GB"),
        "Throughput" => convert(s.value, "MB/s"),
        _ => s.value,
    }
}

pub fn custom_unit(s: &Sensor) -> &'static str {
    match s.typ.as_str() {
        "Temperature" => "°C",
        "Load" | "Control" | "Level" | "Humidity" => "%",
        "Clock" => "MHz",
        "Power" => "W",
        "Voltage" => "V",
        "Current" => "A",
        "Fan" => "RPM",
        "Data" | "SmallData" => "GB",
        "Throughput" => "MB/s",
        "Frequency" => "Hz",
        "Flow" => "L/h",
        "Energy" => "mWh",
        "Noise" => "dBA",
        "Timing" => "ns",
        _ => "",
    }
}

/// Map every standard key to (sensor index, unit) of the LHM sensor that supplies it.
pub fn resolve_standard(sensors: &[Sensor]) -> Vec<(&'static str, usize, &'static str)> {
    // (category, type, name) -> first sensor in LHM tree order
    let mut index: HashMap<(&str, &str, &str), usize> = HashMap::with_capacity(sensors.len());
    for (i, s) in sensors.iter().enumerate() {
        if s.value.is_none() {
            continue;
        }
        index.entry((s.category, s.typ.as_str(), s.name.as_str())).or_insert(i);
    }
    let mut out = Vec::with_capacity(STANDARD_SENSOR_RULES.len());
    for (key, rules) in STANDARD_SENSOR_RULES {
        let mut found = None;
        'rules: for (cats, stype, names, unit) in rules.iter() {
            for name in names.iter() {
                for cat in cats.iter() {
                    if let Some(&i) = index.get(&(*cat, *stype, *name)) {
                        found = Some((i, *unit));
                        break 'rules;
                    }
                }
            }
        }
        if let Some((i, unit)) = found {
            out.push((*key, i, unit));
        }
    }
    out
}

/// Read all standard keys plus the theme's CUSTOM_* keys (custom_map: key -> SensorId).
pub fn read_values(
    sensors: &[Sensor],
    custom_map: &[(String, String)],
    vram_fallback_gb: Option<f64>,
    data: &mut HashMap<String, f64>,
) {
    for (key, i, unit) in resolve_standard(sensors) {
        if let Some(v) = convert(sensors[i].value, unit) {
            data.insert(key.to_string(), v);
        }
    }

    // RAM total = used + available (LHM has no separate 'total' sensor)
    if let (Some(u), Some(f)) = (data.get("RAM_USED_GB").copied(), data.get("RAM_FREE_GB").copied()) {
        data.insert("RAM_TOTAL".into(), u + f);
    }

    // VRAM percentage = used / total, using LHM's own total for the same GPU
    if let Some(used) = data.get("VRAM_USED").copied() {
        let used_hw = sensors
            .iter()
            .find(|s| {
                s.category == "gpu"
                    && (s.name == "D3D Dedicated Memory Used" || s.name == "GPU Memory Used")
                    && s.value.is_some()
            })
            .map(|s| s.hw_id.as_str());
        let mut total = None;
        for want in ["GPU Memory Total", "D3D Dedicated Memory Total"] {
            if let Some(s) = sensors.iter().find(|s| {
                s.category == "gpu"
                    && s.name == want
                    && s.value.map(|v| v != 0.0).unwrap_or(false)
                    && used_hw.map(|h| h == s.hw_id).unwrap_or(true)
            }) {
                total = s.value.map(|v| v / GB);
                break;
            }
        }
        let total = total.filter(|t| *t != 0.0).or(vram_fallback_gb);
        if let Some(t) = total.filter(|t| *t != 0.0) {
            data.insert("VRAM_USAGE".into(), (used / t * 100.0).clamp(0.0, 100.0));
        }
    }

    // Network: use whichever adapter is busiest right now
    for (key, name) in [("NET_DOWN", "Download Speed"), ("NET_UP", "Upload Speed")] {
        let best = sensors
            .iter()
            .filter(|s| s.category == "nic" && s.typ == "Throughput" && s.name == name)
            .filter_map(|s| s.value)
            .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.max(v))));
        if let Some(b) = best {
            data.insert(key.into(), b / MB);
        }
    }

    // CUSTOM_* keys store LHM's stable SensorId (e.g. "/amdcpu/0/temperature/2")
    if !custom_map.is_empty() {
        let by_id: HashMap<&str, &Sensor> = sensors.iter().map(|s| (s.id.as_str(), s)).collect();
        for (key, sid) in custom_map {
            if let Some(s) = by_id.get(sid.as_str()) {
                if let Some(v) = custom_value(s) {
                    data.insert(key.clone(), v);
                }
            }
        }
    }
}

// ── VRAM capacity fallback table ────────────────────────────────────────────
// Only used for GPUs where LHM doesn't expose 'GPU Memory Total'. Matched as
// case-insensitive substrings against LHM's GPU name; the longest match wins.
static GPU_VRAM_GB: &[(&str, f64)] = &[
    ("RX 9070 XT", 16.0),
    ("RX 9070 GRE", 12.0),
    ("RX 9070", 16.0),
    ("RX 9060 XT", 16.0),
    ("RX 7900 XTX", 24.0),
    ("RX 7900 XT", 20.0),
    ("RX 7900 GRE", 16.0),
    ("RX 7800 XT", 16.0),
    ("RX 7700 XT", 12.0),
    ("RX 7600 XT", 16.0),
    ("RX 7600", 8.0),
    ("RTX 4090", 24.0),
    ("RTX 4080 SUPER", 16.0),
    ("RTX 4080", 16.0),
    ("RTX 4070 TI SUPER", 16.0),
    ("RTX 4070 TI", 12.0),
    ("RTX 4070 SUPER", 12.0),
    ("RTX 4070", 12.0),
    ("RTX 4060 TI", 8.0),
    ("RTX 4060", 8.0),
    ("RTX 3090 TI", 24.0),
    ("RTX 3090", 24.0),
    ("RTX 3080 TI", 12.0),
    ("RTX 3080", 10.0),
    ("RTX 3070 TI", 8.0),
    ("RTX 3070", 8.0),
    ("RTX 3060 TI", 8.0),
    ("RTX 3060", 12.0),
    ("2080 TI", 11.0),
    ("2080 SUPER", 8.0),
    ("2080", 8.0),
    ("2070 SUPER", 8.0),
    ("2070", 8.0),
    ("2060 SUPER", 8.0),
    ("2060", 6.0),
];

/// Fallback VRAM capacity lookup by GPU name. Called at startup / re-discovery.
pub fn detect_gpu_vram_capacity(sensors: &[Sensor]) -> Option<f64> {
    let gpu_name = sensors
        .iter()
        .find(|s| s.category == "gpu" && s.hardware.as_deref().map(|h| !h.is_empty()).unwrap_or(false))
        .and_then(|s| s.hardware.clone());
    let gpu_name = match gpu_name {
        Some(n) => n,
        None => {
            crate::log_debug!("VRAM capacity lookup: no discrete GPU found in LibreHardwareMonitor");
            return None;
        }
    };
    let upper = gpu_name.to_uppercase();
    let mut best: Option<(&str, f64)> = None;
    for (key, gb) in GPU_VRAM_GB {
        if upper.contains(&key.to_uppercase()) && best.map(|(b, _)| key.len() > b.len()).unwrap_or(true) {
            best = Some((key, *gb));
        }
    }
    match best {
        Some((k, gb)) => {
            crate::log_info!("VRAM capacity (fallback table): \"{gpu_name}\" matched \"{k}\" -> {gb} GB");
            Some(gb)
        }
        None => {
            crate::log_debug!(
                "VRAM capacity fallback table has no entry for \"{gpu_name}\" (fine as long as LHM reports GPU Memory Total)"
            );
            None
        }
    }
}

// ── Sensor discovery (lhm_sensors.json for the Theme Builder) ───────────────

#[derive(Serialize)]
struct DeviceOut {
    index: usize,
    name: String,
    id: String,
    category: &'static str,
}

#[derive(Serialize)]
struct SensorOut {
    index: usize,
    id: String,
    sensor_index: usize,
    device: Option<String>,
    category: &'static str,
    #[serde(rename = "type")]
    typ: String,
    name: String,
    unit: &'static str,
    sample: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    std_key: Option<&'static str>,
}

#[derive(Serialize)]
struct DiscoveryOut {
    generated: String,
    source: &'static str,
    lhm_version: Option<String>,
    device_names: Vec<DeviceOut>,
    sensors: Vec<SensorOut>,
}

/// Save every LHM sensor to lhm_sensors.json for the Theme Builder's
/// + Sensors picker. Returns (path, sensor count) on success.
pub fn discover_sensors(sensors: &[Sensor], version: Option<String>) -> Option<(std::path::PathBuf, usize)> {
    let mut device_names: Vec<DeviceOut> = Vec::new();
    let mut hw_index: HashMap<&str, usize> = HashMap::new();
    for s in sensors {
        if !hw_index.contains_key(s.hw_id.as_str()) {
            let idx = device_names.len();
            hw_index.insert(s.hw_id.as_str(), idx);
            let name = s.hardware.clone().filter(|h| !h.is_empty()).unwrap_or_else(|| s.hw_id.clone());
            device_names.push(DeviceOut { index: idx, name, id: s.hw_id.clone(), category: s.category });
        }
    }
    let mut std_for: HashMap<usize, &'static str> = HashMap::new();
    for (key, i, _) in resolve_standard(sensors) {
        std_for.insert(i, key);
    }
    let cat_name = |c: &str| -> &'static str {
        match c {
            "cpu" => "cpu",
            "gpu" | "igpu" => "gpu",
            "ram" | "vram" => "ram",
            "mb" => "motherboard",
            "storage" => "storage",
            "nic" => "network",
            _ => "system",
        }
    };
    let mut out_sensors = Vec::new();
    for (i, s) in sensors.iter().enumerate() {
        if s.typ == "TimeSpan" {
            continue; // not a numeric value the panel can render
        }
        let v = custom_value(s);
        out_sensors.push(SensorOut {
            index: i,
            id: s.id.clone(),
            sensor_index: hw_index[s.hw_id.as_str()],
            device: s.hardware.clone(),
            category: cat_name(s.category),
            typ: s.typ.clone(),
            name: s.name.clone(),
            unit: custom_unit(s),
            sample: v.map(|v| (v * 1000.0).round() / 1000.0).filter(|v| v.is_finite()).unwrap_or(0.0),
            std_key: std_for.get(&i).copied(),
        });
    }
    let n = out_sensors.len();
    let nd = device_names.len();
    let out = DiscoveryOut {
        generated: chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
        source: "LibreHardwareMonitor",
        lhm_version: version,
        device_names,
        sensors: out_sensors,
    };
    let path = paths::sensors_file();
    let json = match serde_json::to_string_pretty(&out) {
        Ok(j) => j,
        Err(e) => {
            crate::log_error!("Sensor discovery error: {e}");
            return None;
        }
    };
    if let Err(e) = std::fs::write(&path, json) {
        crate::log_error!("Sensor discovery error: {e}");
        return None;
    }
    crate::log_info!("Sensor discovery: {n} sensors, {nd} devices saved to {}", path.display());
    Some((path, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Lhm {
        Lhm {
            snapshot: None,
            snapshot_time: None,
            version: None,
            fail_since: None,
            fail_logged: false,
            detected_vram_gb: None,
            newly_connected: false,
        }
    }

    fn one_sensor() -> Vec<Sensor> {
        vec![Sensor {
            id: "/amdcpu/0/load/0".into(),
            name: "CPU Total".into(),
            typ: "Load".into(),
            hardware: Some("CPU".into()),
            hw_id: "/amdcpu/0".into(),
            category: "cpu",
            value: Some(10.0),
        }]
    }

    #[test]
    fn keeps_values_through_a_hiccup() {
        crate::logging::set_level("off"); // don't write test output into the real log
        let mut l = fresh();
        assert!(l.current().is_none());
        assert!(l.apply(Ok((one_sensor(), None)), "test").is_some());
        assert!(l.newly_connected);
        l.newly_connected = false;
        // LHM doesn't answer: the last values stay available
        assert!(l.apply(Err("timed out".into()), "test").is_none());
        assert!(l.current().is_some());
        assert!(l.failing_for().is_some());
        assert!(!l.fail_logged, "a short hiccup must not be logged as an outage");
        // LHM answers again
        l.apply(Ok((one_sensor(), None)), "test");
        assert!(l.failing_for().is_none());
        assert!(!l.newly_connected, "recovering from a hiccup is not a new connection");
        // A long outage: snapshot too old -> values go away
        l.snapshot_time = Some(Instant::now() - STALE_LIMIT - Duration::from_secs(1));
        assert!(l.current().is_none());
    }

    #[test]
    fn formatted() {
        assert_eq!(parse_formatted("45.3 °C"), (Some(45.3), "°C".into()));
        assert_eq!(parse_formatted("1,5 GB"), (Some(1.5), "GB".into()));
        assert_eq!(parse_formatted("1,234.5 MHz"), (Some(1234.5), "MHz".into()));
        assert_eq!(parse_formatted("-"), (None, "".into()));
        assert_eq!(parse_formatted("abc"), (None, "".into()));
    }

    #[test]
    fn flatten_tree() {
        let j = r#"{"Text":"Sensor","Children":[{"Text":"PC","Children":[
            {"Text":"AMD Ryzen 7 7800X3D","HardwareId":"/amdcpu/0","Children":[
              {"Text":"Temperatures","Children":[
                {"Text":"Core (Tctl/Tdie)","Value":"45.3 °C","SensorId":"/amdcpu/0/temperature/2","Type":"Temperature","RawValue":45.25}]},
              {"Text":"Load","Children":[
                {"Text":"CPU Total","Value":"12,5 %","SensorId":"/amdcpu/0/load/0"}]}]},
            {"Text":"Generic Memory","Children":[
              {"Text":"Data","Children":[
                {"Text":"Memory Used","Value":"8.0 GB","SensorId":"/ram/data/0"}]}]}]}]}"#;
        let root: Node = serde_json::from_str(j).unwrap();
        let s = flatten(&root);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].category, "cpu");
        assert_eq!(s[0].value, Some(45.25));
        assert_eq!(s[1].typ, "Load");
        assert_eq!(s[1].value, Some(12.5));
        assert_eq!(s[2].category, "ram");
        assert_eq!(s[2].hw_id, "/ram");
        assert_eq!(s[2].value, Some(8.0 * GB));
        let mut d = HashMap::new();
        read_values(&s, &[], None, &mut d);
        assert_eq!(d["CPU_TEMP"], 45.25);
        assert_eq!(d["CPU_USAGE"], 12.5);
        assert_eq!(d["RAM_USED_GB"], 8.0);
    }
}

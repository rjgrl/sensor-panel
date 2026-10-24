//! DS916 serial port detection (USB VID 33C3 / PID F101).

use serialport::SerialPortType;
use std::time::Duration;

pub const VID: u16 = 0x33C3;
pub const PID: u16 = 0xF101;

pub fn detect_ds916_port() -> Option<String> {
    match serialport::available_ports() {
        Ok(ports) => ports.into_iter().find_map(|p| match p.port_type {
            SerialPortType::UsbPort(info) if info.vid == VID && info.pid == PID => Some(p.port_name),
            _ => None,
        }),
        Err(e) => {
            crate::log_error!("Port detection error: {e}");
            None
        }
    }
}

pub fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|v| v.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

pub fn open(name: &str) -> Result<Box<dyn serialport::SerialPort>, String> {
    serialport::new(name, 115_200)
        .timeout(Duration::from_secs(2))
        .open()
        .map_err(|e| e.to_string())
}

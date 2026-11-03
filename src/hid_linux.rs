//! Linux `/dev/hidraw` backend.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::device::{DeviceInfo, Link, Selector};
use crate::hub::Hub;

fn bus_name(bus: u32) -> String {
    match bus {
        0x03 => "usb".into(),
        0x05 => "bluetooth".into(),
        other => format!("bus-{other:04x}"),
    }
}

/// Parse a hidraw `device/uevent` file.
pub fn parse_uevent(path: &str, text: &str) -> Option<DeviceInfo> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .unwrap_or("")
            .to_string()
    };
    let id = field("HID_ID");
    let mut parts = id.split(':').map(|p| u32::from_str_radix(p, 16));
    let (Some(Ok(bus)), Some(Ok(vendor_id)), Some(Ok(product_id)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    Some(DeviceInfo {
        path: path.to_string(),
        vendor_id,
        product_id,
        product: field("HID_NAME"),
        transport: bus_name(bus),
        serial: field("HID_UNIQ"),
        ..Default::default()
    })
}

pub fn enumerate() -> Result<Vec<DeviceInfo>, String> {
    let entries = match std::fs::read_dir("/sys/class/hidraw") {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("/sys/class/hidraw: {e}")),
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    Ok(names
        .iter()
        .filter_map(|name| {
            let uevent = Path::new("/sys/class/hidraw")
                .join(name)
                .join("device/uevent");
            let text = std::fs::read_to_string(uevent).ok()?;
            parse_uevent(&format!("/dev/{name}"), &text)
        })
        .collect())
}

struct HidrawLink {
    file: Mutex<File>,
    info: DeviceInfo,
}

impl Link for HidrawLink {
    fn write(&self, report: &[u8]) -> io::Result<()> {
        let written = self.file.lock().unwrap().write(report)?;
        if written != report.len() {
            return Err(io::Error::other(format!(
                "short HID write: {written}/{}",
                report.len()
            )));
        }
        Ok(())
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }
}

fn connect(selector: &Selector) -> Result<(File, DeviceInfo), String> {
    let devices = enumerate()?;
    let info = devices[selector.select(&devices)?].clone();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&info.path)
        .map_err(|e| format!("{}: {e}", info.path))?;
    Ok((file, info))
}

pub fn start(hub: Arc<Hub>, selector: Selector) {
    thread::Builder::new()
        .name("codex-micro-hid".into())
        .spawn(move || loop {
            match connect(&selector) {
                Ok((mut reader, info)) => match reader.try_clone() {
                    Ok(writer) => {
                        hub.connected(Arc::new(HidrawLink {
                            file: Mutex::new(writer),
                            info,
                        }));
                        // hidraw returns exactly one report per read.
                        let mut buffer = [0u8; 256];
                        let error = loop {
                            match reader.read(&mut buffer) {
                                Ok(0) => break "Codex Micro disconnected".to_string(),
                                Ok(n) => hub.on_report(&buffer[..n]),
                                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                                Err(e) => break format!("Codex Micro disconnected: {e}"),
                            }
                        };
                        hub.disconnected(&error);
                    }
                    Err(e) => hub.disconnected(&e.to_string()),
                },
                Err(error) => hub.disconnected(&error),
            }
            thread::sleep(Duration::from_secs(1));
        })
        .expect("spawn HID thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_and_bluetooth_uevents() {
        let usb = parse_uevent(
            "/dev/hidraw3",
            "DRIVER=hid-generic\nHID_ID=0003:0000303A:00008360\nHID_NAME=Work Louder Codex Micro\nHID_UNIQ=abc\n",
        )
        .unwrap();
        assert_eq!((usb.vendor_id, usb.product_id), (0x303a, 0x8360));
        assert_eq!(usb.transport, "usb");
        assert_eq!(usb.product, "Work Louder Codex Micro");
        assert_eq!(usb.serial, "abc");

        let bt = parse_uevent("/dev/hidraw4", "HID_ID=0005:0000303A:00008360\n").unwrap();
        assert_eq!(bt.transport, "bluetooth");
        assert!(parse_uevent("/dev/hidraw5", "HID_NAME=x\n").is_none());
    }
}

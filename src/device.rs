//! Device identity, selection, and the platform backend interface.

use std::io;
use std::str::FromStr;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::hub::Hub;
use crate::protocol::{PRODUCT_NAME, USB_PRODUCT_ID, USB_VENDOR_ID};

/// One HID device as reported by the operating system.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceInfo {
    pub path: String,
    pub vendor_id: u32,
    pub product_id: u32,
    pub product: String,
    /// `usb`, `bluetooth`, or another lowercase bus name.
    pub transport: String,
    pub serial: String,
    pub max_input_report: u32,
    pub max_output_report: u32,
}

impl DeviceInfo {
    pub fn describe(&self) -> Value {
        json!({
            "path": self.path,
            "vendor_id": format!("{:04x}", self.vendor_id),
            "product_id": format!("{:04x}", self.product_id),
            "product": self.product,
            "transport": self.transport,
            "serial": self.serial,
            "max_input_report": self.max_input_report,
            "max_output_report": self.max_output_report,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransportPreference {
    /// Bluetooth when available, otherwise USB.
    #[default]
    Auto,
    Bluetooth,
    Usb,
}

impl FromStr for TransportPreference {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "bluetooth" => Ok(Self::Bluetooth),
            "usb" => Ok(Self::Usb),
            _ => Err("transport must be one of: auto, bluetooth, usb".into()),
        }
    }
}

/// Which connection to use. A device matches by the USB IDs or by product
/// name, because Bluetooth connections may advertise different IDs.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    pub transport: TransportPreference,
    pub name: String,
}

impl Default for Selector {
    fn default() -> Self {
        Self {
            transport: TransportPreference::Auto,
            name: PRODUCT_NAME.into(),
        }
    }
}

impl Selector {
    pub fn matches(&self, info: &DeviceInfo) -> bool {
        if (info.vendor_id, info.product_id) == (USB_VENDOR_ID, USB_PRODUCT_ID) {
            return true;
        }
        !self.name.is_empty()
            && info
                .product
                .to_lowercase()
                .contains(&self.name.to_lowercase())
    }

    /// Index of the preferred matching device.
    pub fn select(&self, devices: &[DeviceInfo]) -> Result<usize, String> {
        let required = match self.transport {
            TransportPreference::Auto => None,
            TransportPreference::Bluetooth => Some("bluetooth"),
            TransportPreference::Usb => Some("usb"),
        };
        let rank = |info: &DeviceInfo| {
            let transport = match info.transport.as_str() {
                "bluetooth" => 0,
                "usb" => 1,
                _ => 2,
            };
            // The vendor report is the largest output report, so it breaks
            // ties between interfaces of one device.
            (transport, std::cmp::Reverse(info.max_output_report))
        };
        devices
            .iter()
            .enumerate()
            .filter(|(_, info)| self.matches(info))
            .filter(|(_, info)| required.is_none_or(|t| info.transport == t))
            .min_by_key(|(_, info)| rank(info))
            .map(|(index, _)| index)
            .ok_or_else(|| match required {
                Some(t) => format!("Work Louder Codex Micro is not connected over {t}"),
                None => "Work Louder Codex Micro is not connected".into(),
            })
    }
}

/// An open device connection. Reports include the report ID as byte 0.
pub trait Link: Send + Sync {
    fn write(&self, report: &[u8]) -> io::Result<()>;
    fn info(&self) -> &DeviceInfo;
}

/// Enumerate every HID device visible to this process.
pub fn enumerate() -> Result<Vec<DeviceInfo>, String> {
    #[cfg(target_os = "macos")]
    return crate::hid_macos::enumerate();
    #[cfg(target_os = "linux")]
    return crate::hid_linux::enumerate();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err("unsupported platform".into())
}

/// Start the backend thread. It reports connections, input reports, and
/// disconnections to `hub` and reconnects automatically.
pub fn start(hub: Arc<Hub>, selector: Selector) {
    #[cfg(target_os = "macos")]
    crate::hid_macos::start(hub, selector);
    #[cfg(target_os = "linux")]
    crate::hid_linux::start(hub, selector);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = selector;
        hub.disconnected("unsupported platform");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(transport: &str) -> DeviceInfo {
        DeviceInfo {
            path: format!("{transport}-path"),
            vendor_id: USB_VENDOR_ID,
            product_id: USB_PRODUCT_ID,
            product: "Codex Micro".into(),
            transport: transport.into(),
            ..Default::default()
        }
    }

    #[test]
    fn auto_prefers_bluetooth_then_usb() {
        let selector = Selector::default();
        assert_eq!(
            selector.select(&[device("usb"), device("bluetooth")]),
            Ok(1)
        );
        assert_eq!(selector.select(&[device("usb")]), Ok(0));
    }

    #[test]
    fn explicit_transport_is_required() {
        let selector = Selector {
            transport: TransportPreference::Bluetooth,
            ..Default::default()
        };
        let error = selector.select(&[device("usb")]).unwrap_err();
        assert!(error.ends_with("over bluetooth"), "{error}");
    }

    #[test]
    fn matches_by_name_when_ids_differ() {
        let renamed = DeviceInfo {
            vendor_id: 0x02e5,
            product_id: 1,
            product: "Codex Micro BLE".into(),
            ..device("bluetooth")
        };
        let mouse = DeviceInfo {
            vendor_id: 0x05ac,
            product_id: 1,
            product: "Mouse".into(),
            ..device("bluetooth")
        };
        let selector = Selector::default();
        assert!(!selector.matches(&mouse));
        assert_eq!(selector.select(&[mouse, renamed]), Ok(1));
    }

    #[test]
    fn larger_output_report_breaks_ties() {
        let keyboard = DeviceInfo {
            max_output_report: 2,
            ..device("bluetooth")
        };
        let vendor = DeviceInfo {
            max_output_report: 64,
            ..device("bluetooth")
        };
        assert_eq!(Selector::default().select(&[keyboard, vendor]), Ok(1));
    }

    #[test]
    fn parses_transport_names() {
        assert_eq!("usb".parse(), Ok(TransportPreference::Usb));
        assert!("serial".parse::<TransportPreference>().is_err());
    }
}

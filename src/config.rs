//! `config.toml` loading and validation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::device::{Selector, TransportPreference};
use crate::events::JoystickSectors;
use crate::protocol::PRODUCT_NAME;

/// control name -> action (press/release/step) -> argv
pub type Mappings = BTreeMap<String, BTreeMap<String, Vec<String>>>;

pub const MAPPING_ACTIONS: [&str; 3] = ["press", "release", "step"];

#[derive(Debug, Default, PartialEq)]
pub struct Config {
    pub mappings: Mappings,
    pub joystick: JoystickSectors,
    pub selector: Selector,
}

pub fn default_config_path() -> PathBuf {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::home_dir().join(".config"));
    root.join("codex-micro").join("config.toml")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let document: Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        Ok(Self {
            mappings: parse_mappings(document.get("mappings"))?,
            joystick: parse_joystick(document.get("joystick"))?,
            selector: parse_device(document.get("device"))?,
        })
    }
}

fn table<'a>(value: Option<&'a Value>, name: &str) -> Result<Option<&'a Table>, String> {
    match value {
        None => Ok(None),
        Some(Value::Table(t)) => Ok(Some(t)),
        Some(_) => Err(format!("config [{name}] must be a table")),
    }
}

fn reject_unknown(table: &Table, section: &str, allowed: &[&str]) -> Result<(), String> {
    let mut unknown: Vec<&str> = table
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort();
    Err(format!(
        "unknown [{section}] settings: {}",
        unknown.join(", ")
    ))
}

fn parse_mappings(value: Option<&Value>) -> Result<Mappings, String> {
    let mut mappings = Mappings::new();
    let Some(raw) = table(value, "mappings")? else {
        return Ok(mappings);
    };
    for (key, actions) in raw {
        let Value::Table(actions) = actions else {
            return Err(format!("mapping {key} must be a table"));
        };
        let mut parsed = BTreeMap::new();
        for (action, argv) in actions {
            if !MAPPING_ACTIONS.contains(&action.as_str()) {
                return Err(format!("unsupported mapping action: {key}.{action}"));
            }
            let argv: Option<Vec<String>> = match argv {
                Value::Array(parts) if !parts.is_empty() => parts
                    .iter()
                    .map(|p| p.as_str().filter(|s| !s.is_empty()).map(str::to_string))
                    .collect(),
                _ => None,
            };
            let argv =
                argv.ok_or_else(|| format!("{key}.{action} must be a non-empty argv array"))?;
            parsed.insert(action.clone(), argv);
        }
        mappings.insert(key.clone(), parsed);
    }
    Ok(mappings)
}

fn number(table: &Table, key: &str, default: f64) -> Result<f64, String> {
    match table.get(key) {
        None => Ok(default),
        Some(Value::Float(f)) => Ok(*f),
        Some(Value::Integer(i)) => Ok(*i as f64),
        Some(_) => Err(format!("joystick {key} must be a number")),
    }
}

fn parse_joystick(value: Option<&Value>) -> Result<JoystickSectors, String> {
    let Some(raw) = table(value, "joystick")? else {
        return Ok(JoystickSectors::default());
    };
    reject_unknown(raw, "joystick", &["sectors", "offset", "press", "release"])?;
    let defaults = JoystickSectors::default();
    let sectors = match raw.get("sectors") {
        None => defaults.sectors as i64,
        Some(Value::Integer(i)) => *i,
        Some(_) => return Err("joystick sectors must be an integer from 0 through 16".into()),
    };
    JoystickSectors::new(
        sectors,
        number(raw, "offset", defaults.offset)?,
        number(raw, "press", defaults.press)?,
        number(raw, "release", defaults.release)?,
    )
}

fn parse_device(value: Option<&Value>) -> Result<Selector, String> {
    let Some(raw) = table(value, "device")? else {
        return Ok(Selector::default());
    };
    reject_unknown(raw, "device", &["transport", "name"])?;
    let transport = match raw.get("transport") {
        None => TransportPreference::Auto,
        Some(Value::String(s)) => s.parse()?,
        Some(_) => return Err("device transport must be a string".into()),
    };
    let name = match raw.get("name") {
        None => PRODUCT_NAME.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err("device name must be a string".into()),
    };
    Ok(Selector { transport, name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_argv_mappings() {
        let config = Config::parse(
            "[mappings.MIC]\npress = [\"/usr/bin/example\", \"--start\"]\nrelease = [\"/usr/bin/example\", \"--stop\"]\n",
        )
        .unwrap();
        assert_eq!(
            config.mappings["MIC"]["press"],
            ["/usr/bin/example", "--start"]
        );
        assert_eq!(
            config.mappings["MIC"]["release"],
            ["/usr/bin/example", "--stop"]
        );
    }

    #[test]
    fn missing_file_uses_defaults() {
        let config = Config::load(Path::new("/nonexistent/codex-micro/config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn device_and_joystick_sections() {
        let config = Config::parse(
            "[device]\ntransport = \"bluetooth\"\nname = \"Micro\"\n[joystick]\nsectors = 8\noffset = 0.25\npress = 0.7\nrelease = 0.2\n",
        )
        .unwrap();
        assert_eq!(
            config.selector,
            Selector {
                transport: TransportPreference::Bluetooth,
                name: "Micro".into()
            }
        );
        assert_eq!(
            config.joystick,
            JoystickSectors::new(8, 0.25, 0.7, 0.2).unwrap()
        );
    }

    #[test]
    fn rejects_invalid_settings() {
        for text in [
            "[mappings.MIC]\npress = \"example --start\"\n",
            "[mappings.MIC]\npress = []\n",
            "[mappings.AG00]\nhold = [\"/bin/true\"]\n",
            "[device]\ntransport = \"serial\"\n",
            "[device]\nport = 1\n",
            "[joystick]\ndeadzone = 0.1\n",
            "[joystick]\npress = \"high\"\n",
            "[joystick]\nsectors = 4.0\n",
            "mappings = 3\n",
            "not toml",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn example_config_is_valid() {
        let config = Config::parse(include_str!("../config/example.toml")).unwrap();
        for control in ["MIC", "ENC_CW", "ENC_CC", "ENC_CLK", "JOY0"] {
            assert!(config.mappings.contains_key(control), "{control}");
        }
    }
}

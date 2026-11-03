//! Turn vendor notifications into normalized application events.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

const MIC_SWITCHES: [&str; 2] = ["ACT10", "ACT11"];

fn action_name(action: &Value) -> Value {
    match action.as_i64() {
        Some(0) => "release".into(),
        Some(1) => "press".into(),
        Some(2) => "step".into(),
        _ => action.clone(),
    }
}

/// Divide the analog joystick into virtual `JOY0..JOYn` keys.
///
/// `JOY0` is centered on `offset`; later sectors follow in increasing-angle
/// order. A sector presses at `press` distance and releases below `release`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JoystickSectors {
    pub sectors: u32,
    pub offset: f64,
    pub press: f64,
    pub release: f64,
}

impl Default for JoystickSectors {
    fn default() -> Self {
        Self {
            sectors: 4,
            offset: 0.0,
            press: 0.6,
            release: 0.4,
        }
    }
}

impl JoystickSectors {
    pub fn new(sectors: i64, offset: f64, press: f64, release: f64) -> Result<Self, String> {
        if !(0..=16).contains(&sectors) {
            return Err("joystick sectors must be an integer from 0 through 16".into());
        }
        for (name, value) in [("offset", offset), ("press", press), ("release", release)] {
            if !(0.0..=1.0).contains(&value) {
                return Err(format!("joystick {name} must be between 0 and 1"));
            }
        }
        if release > press {
            return Err("joystick release must not exceed press".into());
        }
        Ok(Self {
            sectors: sectors as u32,
            offset,
            press,
            release,
        })
    }

    pub fn sector(&self, angle: f64) -> u32 {
        let width = 1.0 / self.sectors as f64;
        ((angle - self.offset + width / 2.0).rem_euclid(1.0) / width) as u32 % self.sectors
    }
}

#[derive(Default)]
pub struct Normalizer {
    mic_down: BTreeSet<String>,
    joystick: JoystickSectors,
    active_sector: Option<u32>,
}

impl Normalizer {
    pub fn new(joystick: JoystickSectors) -> Self {
        Self {
            joystick,
            ..Self::default()
        }
    }

    /// Forget held controls, e.g. after a reconnect.
    pub fn reset(&mut self) {
        self.mic_down.clear();
        self.active_sector = None;
    }

    pub fn set_joystick(&mut self, joystick: JoystickSectors) {
        self.joystick = joystick;
        self.active_sector = None;
    }

    pub fn notification(&mut self, message: &Map<String, Value>) -> Vec<Value> {
        let empty = Map::new();
        let params = message
            .get("p")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        match message.get("m").and_then(Value::as_str) {
            Some("v.oai.hid") => self.hid(params),
            Some("v.oai.rad") => self.joystick(params),
            _ => vec![json!({
                "event": "vendor",
                "data": {
                    "method": message.get("m").cloned().unwrap_or(Value::Null),
                    "params": params,
                },
            })],
        }
    }

    fn hid(&mut self, params: &Map<String, Value>) -> Vec<Value> {
        let key = match params.get("k") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let action = params.get("act").cloned().unwrap_or(Value::Null);
        let data = json!({
            "key": key,
            "action": action_name(&action),
            "action_code": action,
        });
        if key.starts_with("ENC_") {
            return vec![json!({"event": "encoder", "data": data})];
        }
        if MIC_SWITCHES.contains(&key.as_str()) {
            let mut events = vec![json!({"event": "key.raw", "data": data})];
            events.extend(self.mic_switch(&key, action.as_i64()));
            return events;
        }
        vec![json!({"event": "key", "data": data})]
    }

    /// The wide Mic keycap closes ACT10 and/or ACT11; report one logical key.
    fn mic_switch(&mut self, key: &str, action: Option<i64>) -> Option<Value> {
        let was_down = !self.mic_down.is_empty();
        match action {
            Some(1) => {
                self.mic_down.insert(key.to_string());
            }
            Some(0) => {
                self.mic_down.remove(key);
            }
            _ => {}
        }
        let (name, code) = match (was_down, !self.mic_down.is_empty()) {
            (false, true) => ("press", 1),
            (true, false) => ("release", 0),
            _ => return None,
        };
        Some(json!({
            "event": "key",
            "data": {
                "key": "MIC",
                "action": name,
                "action_code": code,
                "raw_keys": MIC_SWITCHES,
            },
        }))
    }

    fn joystick(&mut self, params: &Map<String, Value>) -> Vec<Value> {
        let angle = params.get("a").cloned().unwrap_or(Value::Null);
        let distance = params.get("d").cloned().unwrap_or(Value::Null);
        let mut events = vec![json!({
            "event": "joystick",
            "data": {"angle": angle, "distance": distance},
        })];
        if self.joystick.sectors == 0 {
            return events;
        }
        let (Some(angle), Some(distance)) = (angle.as_f64(), distance.as_f64()) else {
            return events;
        };
        let active = self.active_sector;
        if distance >= self.joystick.press {
            let sector = self.joystick.sector(angle);
            if Some(sector) != active {
                if let Some(previous) = active {
                    events.push(sector_event(previous, 0));
                }
                self.active_sector = Some(sector);
                events.push(sector_event(sector, 1));
            }
        } else if distance < self.joystick.release {
            if let Some(previous) = active {
                self.active_sector = None;
                events.push(sector_event(previous, 0));
            }
        }
        events
    }
}

fn sector_event(sector: u32, action: i64) -> Value {
    json!({
        "event": "joystick.sector",
        "data": {
            "key": format!("JOY{sector}"),
            "action": action_name(&action.into()),
            "action_code": action,
            "sector": sector,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(normalizer: &mut Normalizer, messages: &[Value]) -> Vec<Value> {
        messages
            .iter()
            .flat_map(|m| normalizer.notification(m.as_object().unwrap()))
            .collect()
    }

    fn hid(key: &str, act: i64) -> Value {
        json!({"m": "v.oai.hid", "p": {"k": key, "act": act}})
    }

    fn logical(events: &[Value], kind: &str) -> Vec<(String, String)> {
        events
            .iter()
            .filter(|e| e["event"] == kind)
            .map(|e| {
                (
                    e["data"]["key"].as_str().unwrap().to_string(),
                    e["data"]["action"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn two_switch_mic_key_becomes_one_press_and_release() {
        let mut n = Normalizer::default();
        let events = feed(
            &mut n,
            &[
                hid("ACT10", 1),
                hid("ACT11", 1),
                hid("ACT10", 0),
                hid("ACT11", 0),
            ],
        );
        assert_eq!(
            logical(&events, "key"),
            [
                ("MIC".into(), "press".into()),
                ("MIC".into(), "release".into())
            ]
        );
        assert_eq!(logical(&events, "key.raw").len(), 4);
    }

    #[test]
    fn either_side_operates_the_wide_key() {
        let mut n = Normalizer::default();
        let events = feed(&mut n, &[hid("ACT11", 1), hid("ACT11", 0)]);
        assert_eq!(logical(&events, "key").len(), 2);
    }

    #[test]
    fn encoder_and_joystick_are_normalized() {
        let mut n = Normalizer::new(JoystickSectors {
            sectors: 0,
            ..Default::default()
        });
        let events = feed(
            &mut n,
            &[
                hid("ENC_CW", 2),
                json!({"m": "v.oai.rad", "p": {"a": 0.75, "d": 1}}),
            ],
        );
        assert_eq!(
            Value::Array(events),
            json!([
                {"event": "encoder", "data": {"key": "ENC_CW", "action": "step", "action_code": 2}},
                {"event": "joystick", "data": {"angle": 0.75, "distance": 1}},
            ])
        );
    }

    #[test]
    fn unknown_notifications_are_preserved() {
        let mut n = Normalizer::default();
        let events = feed(&mut n, &[json!({"m": "future.event", "p": {"value": 3}})]);
        assert_eq!(
            events[0],
            json!({"event": "vendor", "data": {"method": "future.event", "params": {"value": 3}}})
        );
    }

    #[test]
    fn sector_boundaries_are_centered_on_offset() {
        let s = JoystickSectors::new(4, 0.1, 0.6, 0.4).unwrap();
        assert_eq!(
            [0.1, 0.99, 0.2, 0.24, 0.6, 0.85].map(|a| s.sector(a)),
            [0, 0, 0, 1, 2, 3]
        );
    }

    #[test]
    fn joystick_press_move_and_release_with_hysteresis() {
        let mut n = Normalizer::default();
        let rad = |a: f64, d: f64| json!({"m": "v.oai.rad", "p": {"a": a, "d": d}});
        let events = feed(
            &mut n,
            &[
                rad(0.0, 0.3),
                rad(0.0, 0.9),
                rad(0.02, 0.5),
                rad(0.25, 1.0),
                rad(0.25, 0.1),
                rad(0.0, 0.0),
            ],
        );
        assert_eq!(
            logical(&events, "joystick.sector"),
            [
                ("JOY0".into(), "press".into()),
                ("JOY0".into(), "release".into()),
                ("JOY1".into(), "press".into()),
                ("JOY1".into(), "release".into()),
            ]
        );
    }

    #[test]
    fn invalid_joystick_settings_are_rejected() {
        assert!(JoystickSectors::new(17, 0.0, 0.6, 0.4).is_err());
        assert!(JoystickSectors::new(4, 0.0, 1.5, 0.4).is_err());
        assert!(JoystickSectors::new(4, 0.0, 0.3, 0.5).is_err());
    }
}

//! Report-6 framing, the streaming JSON-RPC decoder, and lighting payloads.

use serde_json::{json, Map, Value};

pub const USB_VENDOR_ID: u32 = 0x303a;
pub const USB_PRODUCT_ID: u32 = 0x8360;
pub const PRODUCT_NAME: &str = "Codex Micro";
pub const REPORT_ID: u8 = 6;
pub const LOG_CHANNEL: u8 = 1;
pub const RPC_CHANNEL: u8 = 2;
pub const REPORT_SIZE: usize = 64;
pub const MAX_PAYLOAD: usize = 61;
const MAX_RPC_BUFFER: usize = 1024 * 1024;

pub const EFFECTS: [(&str, u8); 7] = [
    ("off", 0),
    ("solid", 1),
    ("snake", 2),
    ("rainbow", 3),
    ("breath", 4),
    ("gradient", 5),
    ("shallow-breath", 6),
];

pub fn effect_code(name: &str) -> Option<u8> {
    EFFECTS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// Split one JSON-RPC request into zero-padded 64-byte report-6 packets.
pub fn encode_rpc(id: u64, method: &str, params: Option<&Value>) -> Vec<[u8; REPORT_SIZE]> {
    let mut request = Map::new();
    request.insert("method".into(), Value::from(method));
    request.insert("id".into(), Value::from(id));
    if let Some(params) = params {
        request.insert("params".into(), params.clone());
    }
    let encoded = Value::Object(request).to_string().into_bytes();
    encoded
        .chunks(MAX_PAYLOAD)
        .map(|chunk| {
            let mut report = [0u8; REPORT_SIZE];
            report[0] = REPORT_ID;
            report[1] = RPC_CHANNEL;
            report[2] = chunk.len() as u8;
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect()
}

/// The JSON-RPC payload bytes of a report, if it is a report-6 RPC fragment.
pub fn rpc_payload(report: &[u8]) -> Option<&[u8]> {
    if report.len() < 3 || report[0] != REPORT_ID || report[1] != RPC_CHANNEL {
        return None;
    }
    let size = report[2] as usize;
    if size > MAX_PAYLOAD {
        return None;
    }
    Some(&report[3..report.len().min(3 + size)])
}

/// Reassembles JSON objects that span reports, including split UTF-8.
#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
    text: String,
}

impl Decoder {
    pub fn reset(&mut self) {
        self.bytes.clear();
        self.text.clear();
    }

    fn decode_utf8(&mut self) {
        let mut start = 0;
        loop {
            match std::str::from_utf8(&self.bytes[start..]) {
                Ok(valid) => {
                    self.text.push_str(valid);
                    start = self.bytes.len();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    // SAFETY-free: the prefix was just validated.
                    let prefix = std::str::from_utf8(&self.bytes[start..start + valid]).unwrap();
                    self.text.push_str(prefix);
                    match error.error_len() {
                        Some(invalid) => {
                            self.text.push('\u{fffd}');
                            start += valid + invalid;
                        }
                        None => {
                            start += valid; // incomplete trailing code point
                            break;
                        }
                    }
                }
            }
        }
        self.bytes.drain(..start);
    }

    /// Append payload bytes and return every complete JSON object.
    pub fn push(&mut self, payload: &[u8]) -> Result<Vec<Map<String, Value>>, String> {
        self.bytes.extend_from_slice(payload);
        self.decode_utf8();
        if self.text.len() > MAX_RPC_BUFFER {
            self.reset();
            return Err("RPC receive buffer exceeded 1 MiB".into());
        }
        let mut messages = Vec::new();
        let mut consumed = 0;
        let mut stream = serde_json::Deserializer::from_str(&self.text).into_iter::<Value>();
        loop {
            match stream.next() {
                Some(Ok(value)) => {
                    consumed = stream.byte_offset();
                    if let Value::Object(object) = value {
                        messages.push(object);
                    }
                }
                Some(Err(_)) => break, // incomplete; wait for more reports
                None => {
                    consumed = self.text.len();
                    break;
                }
            }
        }
        self.text.drain(..consumed);
        Ok(messages)
    }
}

/// JSON number that keeps integral values as integers, as the firmware saw
/// from the original client.
pub fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

pub fn parse_hex_color(text: &str) -> Option<u32> {
    let cleaned = text.strip_prefix('#').unwrap_or(text);
    if cleaned.len() != 6 || !cleaned.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(cleaned, 16).ok()
}

pub fn rgb(value: Option<&Value>) -> Result<u32, String> {
    let parsed = match value {
        Some(Value::Number(n)) => n.as_u64().filter(|v| *v <= 0xff_ffff).map(|v| v as u32),
        Some(Value::String(s)) => parse_hex_color(s),
        _ => None,
    };
    parsed.ok_or_else(|| "color must be #RRGGBB or an integer from 0 through 16777215".into())
}

pub fn unit_interval(value: Option<&Value>, name: &str, default: f64) -> Result<f64, String> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(f64::NAN);
            if (0.0..=1.0).contains(&v) {
                Ok(v)
            } else {
                Err(format!("{name} must be between 0 and 1"))
            }
        }
        Some(_) => Err(format!("{name} must be a number between 0 and 1")),
    }
}

fn effect(value: Option<&Value>) -> Result<(&str, u8), String> {
    let name = match value {
        None | Some(Value::Null) => "solid",
        Some(Value::String(s)) => s.as_str(),
        Some(other) => return Err(format!("unknown effect: {other}")),
    };
    effect_code(name)
        .map(|code| (name, code))
        .ok_or_else(|| format!("unknown effect: {name}"))
}

/// Validate an API agent object into a `v.oai.thstatus` entry.
pub fn agent_payload(params: &Value) -> Result<Value, String> {
    let Value::Object(p) = params else {
        return Err("each agent must be an object".into());
    };
    let id = match p.get("id") {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
    .unwrap_or(-1);
    if !(0..6).contains(&id) {
        return Err("agent id must be from 0 through 5".into());
    }
    let brightness = unit_interval(p.get("brightness"), "brightness", 1.0)?;
    let speed = unit_interval(p.get("speed"), "speed", 0.4)?;
    let color = rgb(p.get("color"))?;
    let (_, code) = effect(p.get("effect"))?;
    Ok(json!({
        "id": id,
        "c": color,
        "b": number(brightness),
        "e": code,
        "s": number(speed),
        "sk": 0,
        "sa": 0,
    }))
}

/// Validate an API zone object into a `v.oai.rgbcfg` side.
pub fn zone_payload(params: &Map<String, Value>) -> Result<Value, String> {
    let brightness = unit_interval(params.get("brightness"), "brightness", 1.0)?;
    let (name, code) = effect(params.get("effect"))?;
    let color = rgb(params.get("color"))?;
    let default_speed = if matches!(name, "off" | "solid") {
        0.0
    } else {
        0.4
    };
    let speed = unit_interval(params.get("speed"), "speed", default_speed)?;
    let magic = unit_interval(params.get("magic"), "magic", 0.0)?;
    Ok(json!({
        "e": code,
        "b": number(brightness),
        "s": number(speed),
        "m": number(magic),
        "c": color,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_requests_into_padded_reports() {
        let params = json!({"text": "x".repeat(100)});
        let reports = encode_rpc(7, "example.method", Some(&params));
        assert!(reports.len() > 1);
        let mut joined = Vec::new();
        for report in &reports {
            assert_eq!(&report[..2], &[6, 2]);
            assert!(report[2] as usize <= MAX_PAYLOAD);
            joined.extend_from_slice(&report[3..3 + report[2] as usize]);
        }
        let text = String::from_utf8(joined).unwrap();
        assert!(text.starts_with(r#"{"method":"example.method","id":7,"params":"#));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["params"], params);
    }

    #[test]
    fn decodes_fragmented_utf8_and_json() {
        let mut decoder = Decoder::default();
        let encoded = r#"{"id":1,"result":{"label":"café"}}"#.as_bytes();
        let split = encoded.iter().position(|b| *b == 0xc3).unwrap() + 1;

        assert!(decoder.push(&encoded[..split]).unwrap().is_empty());
        let messages = decoder.push(&encoded[split..]).unwrap();
        assert_eq!(
            Value::Object(messages[0].clone()),
            json!({"id":1,"result":{"label":"café"}})
        );
    }

    #[test]
    fn decodes_concatenated_messages_and_keeps_partial_tail() {
        let mut decoder = Decoder::default();
        let first = decoder
            .push(b"{\"id\":1,\"result\":true}\r\n{\"m\":\"event\",\"p\":{}}{\"id\":2")
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[1]["m"], "event");
        let second = decoder.push(b",\"result\":false}").unwrap();
        assert_eq!(second[0]["id"], 2);
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        let mut decoder = Decoder::default();
        let messages = decoder.push(b"{\"m\":\"a\xffb\"}").unwrap();
        assert_eq!(messages[0]["m"], "a\u{fffd}b");
    }

    #[test]
    fn rpc_payload_filters_other_reports() {
        assert_eq!(rpc_payload(&[6, 2, 2, b'{', b'}', 0]), Some(&b"{}"[..]));
        assert_eq!(rpc_payload(&[6, 1, 2, b'h', b'i']), None);
        assert_eq!(rpc_payload(&[1, 0, 4]), None);
        assert_eq!(rpc_payload(&[6, 2, 62]), None);
    }

    #[test]
    fn agent_payload_matches_wire_shape() {
        let payload = agent_payload(&json!({
            "id": 5, "color": "#12abef", "brightness": 0.25,
            "effect": "shallow-breath", "speed": 0.75
        }))
        .unwrap();
        assert_eq!(
            payload.to_string(),
            r#"{"id":5,"c":1223663,"b":0.25,"e":6,"s":0.75,"sk":0,"sa":0}"#
        );
    }

    #[test]
    fn zone_payload_uses_effect_defaults() {
        let breath = json!({"color": 0x010203, "brightness": 0.5, "effect": "breath"});
        assert_eq!(
            zone_payload(breath.as_object().unwrap())
                .unwrap()
                .to_string(),
            r#"{"e":4,"b":0.5,"s":0.4,"m":0,"c":66051}"#
        );
        let solid = json!({"color": "#000000"});
        assert_eq!(zone_payload(solid.as_object().unwrap()).unwrap()["s"], 0);
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(rgb(Some(&json!("#nope00"))).is_err());
        assert!(rgb(Some(&json!(0x1000000))).is_err());
        assert!(rgb(None).is_err());
        assert!(unit_interval(Some(&json!(1.1)), "brightness", 1.0).is_err());
        assert!(agent_payload(&json!({"id": 6, "color": "#ffffff"})).is_err());
        assert!(
            agent_payload(&json!({"id": 0, "color": "#ffffff", "effect": "sparkles"})).is_err()
        );
        assert_eq!(parse_hex_color("A0b1C2"), Some(0xa0b1c2));
        assert_eq!(parse_hex_color("#fff"), None);
    }
}

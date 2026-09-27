//! Playback commands the receiver sends back over the AP2 events channel.
//!
//! When the receiver asks for play/pause/next/previous while streaming,
//! it forwards it to the sender as an events-channel `POST /command` whose bplist body is
//! `{type: "sendMediaRemoteCommand", value: "nitm", modernMediaRemoteCommand: "4", params: {...}}`.
//! `value` is the DACP-style four-letter code, `modernMediaRemoteCommand` the zero-based
//! MediaRemote command number. It does not use DACP HTTP callbacks.
//! Receiver commands: `play`/0, `paus`/1, `nitm`/4, `pitm`/5. Volume changes arrive as
//! `value: "dvlc"` with a 0–1 `volume` (and optional `params.volume`) notification.

use plist::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    /// Receiver-initiated volume update, normalized to 0.0..=1.0.
    Volume(f32),
}

/// What an events-channel request body means for playback control.
#[derive(Debug, PartialEq)]
pub enum Event {
    Command(Command),
    /// A `sendMediaRemoteCommand` we do not map (yet); the fields are kept for the log.
    Unsupported {
        value: Option<String>,
        number: Option<String>,
    },
    /// Anything else, e.g. the `updateInfo` sent right after SETUP.
    Other,
}

fn by_code(code: &str) -> Option<Command> {
    Some(match code {
        "play" => Command::Play,
        "paus" => Command::Pause,
        "playpause" | "pply" => Command::PlayPause,
        // Stop has no separate system-media action worth sending; pausing keeps the player.
        "stop" => Command::Pause,
        "nitm" => Command::Next,
        "pitm" => Command::Previous,
        _ => return None,
    })
}

fn by_number(number: &str) -> Option<Command> {
    Some(match number.trim().parse::<u32>().ok()? {
        0 => Command::Play,
        1 | 3 => Command::Pause,
        2 => Command::PlayPause,
        4 => Command::Next,
        5 => Command::Previous,
        _ => return None,
    })
}

fn extract_volume(dict: &plist::Dictionary) -> Option<f32> {
    let raw = dict.get("volume").or_else(|| {
        dict.get("params")
            .and_then(Value::as_dictionary)
            .and_then(|p| p.get("volume"))
    })?;
    let val = match raw {
        Value::Real(r) => *r as f32,
        Value::Integer(i) => {
            if let Some(u) = i.as_unsigned() {
                u as f32
            } else if let Some(s) = i.as_signed() {
                s as f32
            } else {
                return None;
            }
        }
        Value::String(s) => s.trim().parse::<f32>().ok()?,
        _ => return None,
    };
    if val.is_finite() {
        Some(val.clamp(0.0, 1.0))
    } else {
        None
    }
}

/// Interprets one decoded events-channel body.
pub fn parse_event(body: &Value) -> Event {
    let Some(dict) = body.as_dictionary() else {
        return Event::Other;
    };
    if dict.get("type").and_then(Value::as_string) != Some("sendMediaRemoteCommand") {
        return Event::Other;
    }
    let text = |key: &str| match dict.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Integer(i)) => Some(i.to_string()),
        _ => None,
    };
    let value = text("value");
    let number = text("modernMediaRemoteCommand");
    if value.as_deref() == Some("dvlc") {
        if let Some(vol) = extract_volume(dict) {
            return Event::Command(Command::Volume(vol));
        }
        return Event::Unsupported { value, number };
    }
    // The number is the primary identifier; the code is a fallback when it is missing
    // or unknown to us.
    match number
        .as_deref()
        .and_then(by_number)
        .or_else(|| value.as_deref().and_then(by_code))
    {
        Some(command) => Event::Command(command),
        None => Event::Unsupported { value, number },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Dictionary;

    fn command(value: Option<&str>, number: Option<Value>) -> Value {
        let mut d = Dictionary::new();
        d.insert("type".into(), "sendMediaRemoteCommand".into());
        if let Some(v) = value {
            d.insert("value".into(), v.into());
        }
        if let Some(n) = number {
            d.insert("modernMediaRemoteCommand".into(), n);
        }
        d.insert("params".into(), Value::Dictionary(Dictionary::new()));
        Value::Dictionary(d)
    }

    #[test]
    fn captured_homepod_commands() {
        // Commands captured from a tvOS AirPlay 2 receiver.
        for (value, number, expected) in [
            ("play", "0", Command::Play),
            ("paus", "1", Command::Pause),
            ("nitm", "4", Command::Next),
            ("pitm", "5", Command::Previous),
        ] {
            assert_eq!(
                parse_event(&command(Some(value), Some(number.into()))),
                Event::Command(expected)
            );
        }
    }

    #[test]
    fn number_or_code_alone_and_integer_numbers() {
        assert_eq!(
            parse_event(&command(Some("play"), None)),
            Event::Command(Command::Play)
        );
        assert_eq!(
            parse_event(&command(None, Some(Value::Integer(2.into())))),
            Event::Command(Command::PlayPause)
        );
        // An unknown number falls back to a known code.
        assert_eq!(
            parse_event(&command(Some("nitm"), Some("999".into()))),
            Event::Command(Command::Next)
        );
    }

    #[test]
    fn volume_dvlc_command() {
        // Real shape captured from an AirPlay 2 receiver:
        // {type: "sendMediaRemoteCommand", value: "dvlc", volume: Real(0.85), params: {volume: Real(0.85)}}
        let mut d = Dictionary::new();
        d.insert("type".into(), "sendMediaRemoteCommand".into());
        d.insert("value".into(), "dvlc".into());
        d.insert("volume".into(), Value::Real(0.85));
        let mut params = Dictionary::new();
        params.insert("volume".into(), Value::Real(0.85));
        d.insert("params".into(), Value::Dictionary(params));
        assert_eq!(
            parse_event(&Value::Dictionary(d)),
            Event::Command(Command::Volume(0.85))
        );

        // Fallback to params.volume when top-level volume is missing
        let mut d = Dictionary::new();
        d.insert("type".into(), "sendMediaRemoteCommand".into());
        d.insert("value".into(), "dvlc".into());
        let mut params = Dictionary::new();
        params.insert("volume".into(), Value::Real(0.42));
        d.insert("params".into(), Value::Dictionary(params));
        assert_eq!(
            parse_event(&Value::Dictionary(d)),
            Event::Command(Command::Volume(0.42))
        );

        // Out-of-bounds volume is clamped to 0.0..=1.0
        let mut d = Dictionary::new();
        d.insert("type".into(), "sendMediaRemoteCommand".into());
        d.insert("value".into(), "dvlc".into());
        d.insert("volume".into(), Value::Real(1.5));
        assert_eq!(
            parse_event(&Value::Dictionary(d)),
            Event::Command(Command::Volume(1.0))
        );

        let mut d = Dictionary::new();
        d.insert("type".into(), "sendMediaRemoteCommand".into());
        d.insert("value".into(), "dvlc".into());
        d.insert("volume".into(), Value::Real(-0.2));
        assert_eq!(
            parse_event(&Value::Dictionary(d)),
            Event::Command(Command::Volume(0.0))
        );

        // Non-finite volume is dropped to Unsupported
        let mut d = Dictionary::new();
        d.insert("type".into(), "sendMediaRemoteCommand".into());
        d.insert("value".into(), "dvlc".into());
        d.insert("volume".into(), Value::Real(f64::NAN));
        assert_eq!(
            parse_event(&Value::Dictionary(d)),
            Event::Unsupported {
                value: Some("dvlc".into()),
                number: None
            }
        );

        // Missing volume is dropped to Unsupported
        let mut d = Dictionary::new();
        d.insert("type".into(), "sendMediaRemoteCommand".into());
        d.insert("value".into(), "dvlc".into());
        assert_eq!(
            parse_event(&Value::Dictionary(d)),
            Event::Unsupported {
                value: Some("dvlc".into()),
                number: None
            }
        );
    }

    #[test]
    fn unsupported_and_other_events() {
        assert_eq!(
            parse_event(&command(Some("skfw"), Some("17".into()))),
            Event::Unsupported {
                value: Some("skfw".into()),
                number: Some("17".into())
            }
        );
        let mut info = Dictionary::new();
        info.insert("type".into(), "updateInfo".into());
        assert_eq!(parse_event(&Value::Dictionary(info)), Event::Other);
        assert_eq!(parse_event(&Value::String("x".into())), Event::Other);
    }
}

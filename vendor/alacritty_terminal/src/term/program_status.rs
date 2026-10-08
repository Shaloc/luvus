//! Bounded OSC 7501 records (revision 0.2). These are untrusted terminal
//! metadata, never process identity or permission to execute a command.

use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine;

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
const MAX_RECORDS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Working => 3,
            Self::Error => 2,
            Self::Done => 1,
            Self::Idle => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub state: State,
    pub app: Option<String>,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// Update order also provides the bounded eviction policy. A report replaces
/// one complete record; validation always finishes before any mutation.
#[derive(Default)]
pub struct ProgramStatus {
    records: Vec<Record>,
}

impl ProgramStatus {
    pub fn report(&mut self, body: &[u8]) {
        let Some((record, clear)) = parse(body) else {
            return;
        };
        if clear {
            self.records
                .retain(|r| !descendant_or_same(&r.id, &record.id));
        } else {
            self.records.retain(|r| r.id != record.id);
            if self.records.len() == MAX_RECORDS {
                self.records.remove(0);
            }
            self.records.push(record);
        }
    }

    pub fn reset(&mut self) {
        self.records.clear();
    }

    /// OSC 133 A or exit of the process attached to this terminal. Completed
    /// results survive; live states must not leak into the next shell command.
    pub fn end_command(&mut self) {
        self.records
            .retain(|r| matches!(r.state, State::Done | State::Error));
    }

    /// Attention first, then running work, completed results, and idle. Newest
    /// wins ties. Parent records need not exist for inheritance or clearing.
    pub fn active(&self) -> Option<Record> {
        let mut record = self
            .records
            .iter()
            .max_by_key(|r| r.state.priority())?
            .clone();
        if record.app.is_none() {
            record.app = self
                .records
                .iter()
                .filter(|r| r.app.is_some() && descendant_or_same(&record.id, &r.id))
                .max_by_key(|r| r.id.len())
                .and_then(|r| r.app.clone());
        }
        Some(record)
    }
}

fn descendant_or_same(id: &str, parent: &str) -> bool {
    parent.is_empty() || id == parent || id.strip_prefix(parent).is_some_and(|s| s.starts_with('/'))
}

fn segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

fn text(value: &str, limit: usize) -> Option<String> {
    let bytes = BASE64.decode(value).ok()?;
    if bytes.len() > limit {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    // Disarm invisible formatting in chrome. Do not interpret prose or markup.
    Some(
        text.chars()
            .filter(|c| {
                !matches!(*c,
                    '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' |
                    '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'
                )
            })
            .collect(),
    )
}

fn parse(body: &[u8]) -> Option<(Record, bool)> {
    // Reserve OSC + selector + ST even for BEL-terminated reports.
    if body.len() > 4096 - 9 {
        return None;
    }
    let mut state = None;
    let mut id = None;
    let mut app = None;
    let mut kind = None;
    let mut progress = None;
    let mut title = None;
    let mut msg = None;
    for pair in body.split(|b| *b == b':') {
        let equals = pair.iter().position(|b| *b == b'=');
        let (raw_key, raw_value) =
            equals.map_or((pair, &b""[..]), |i| (&pair[..i], &pair[i + 1..]));
        let (raw_key, raw_value) = (raw_key.trim_ascii(), raw_value.trim_ascii());
        let key = std::str::from_utf8(raw_key).unwrap_or("");
        // Every pair's limits are checked, including overwritten/unknown pairs.
        if raw_key.len() > 16
            || match key {
                "msg" => raw_value.len() > 2732,
                "title" => raw_value.len() > 256,
                "app" => raw_value.len() > 32,
                "id" => {
                    raw_value.len() > 128
                        || raw_value.split(|b| *b == b'/').count() > 8
                        || raw_value.split(|b| *b == b'/').any(|p| p.len() > 32)
                }
                _ => false,
            }
        {
            return None;
        }
        // A malformed id must never silently address the root. Retain its raw
        // value for final validation; duplicate keys still use the last value.
        if key == "id" && equals.is_some() {
            id = Some(raw_value);
        }
        let Ok(value) = std::str::from_utf8(raw_value) else {
            continue;
        };
        if equals.is_none()
            || key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_lowercase())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b))
        {
            continue;
        }
        match key {
            "state" => state = Some(value),
            "id" => {}
            "app" => app = segment(value).then(|| value.to_owned()),
            "kind" => {
                kind = matches!(value, "permission" | "question" | "auth").then(|| value.to_owned())
            }
            "progress" => {
                progress = (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| value.parse::<u8>().ok().filter(|p| *p <= 100))
                    .flatten()
            }
            "title" => title = Some(text(value, 192)?),
            "msg" => msg = Some(text(value, 2048)?),
            _ => {}
        }
    }
    let clear = state? == "clear";
    let state = match state? {
        "idle" | "clear" => State::Idle,
        "working" => State::Working,
        "done" => State::Done,
        "blocked" => State::Blocked,
        "error" => State::Error,
        _ => return None,
    };
    let id = match id {
        Some(id) => Some(std::str::from_utf8(id).ok()?),
        None => None,
    };
    if id.is_some_and(|id| !id.split('/').all(segment)) {
        return None;
    }
    if state != State::Blocked {
        kind = None;
    }
    if !matches!(state, State::Working | State::Blocked) {
        progress = None;
    }
    Some((
        Record {
            id: id.unwrap_or("").into(),
            state,
            app,
            kind,
            progress,
            title,
            msg,
        },
        clear,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy_replacement_inheritance_and_clear() {
        let mut s = ProgramStatus::default();
        s.report(b"state=working:app=codex:msg=SGVsbG8=");
        s.report(b"state=blocked:id=build/test:kind=permission:progress=42");
        let r = s.active().unwrap();
        assert_eq!(r.app.as_deref(), Some("codex"));
        assert_eq!(r.progress, Some(42));
        s.report(b"state=idle:id=build/test");
        assert_eq!(s.records[1].kind, None);
        s.report(b"state=error:id=builder");
        s.report(b"state=clear:id=build");
        assert_eq!(s.records.len(), 2);
        s.end_command();
        assert_eq!(s.active().unwrap().id, "builder");
        s.report(b"state=clear");
        assert!(s.active().is_none());
    }

    #[test]
    fn malformed_limits_and_text_reject_atomically() {
        let mut s = ProgramStatus::default();
        s.report(b"state=working");
        for bad in [
            "state=wat",
            "id=child",
            "state=done:id=",
            "state=done:id=a//b",
            "state=done:msg=AA==",
            "state=done:msg=wog=",
            "state=done:title=/w==",
            "state=done:msg=A:msg=",
            "state=done:title=SGVsbG8===",
            "state=done:id=/a",
        ] {
            s.report(bad.as_bytes());
            assert_eq!(s.active().unwrap().state, State::Working, "{bad}");
        }
        for bad in [&b"state=idle:id=a$"[..], b"state=idle:id=\xff"] {
            s.report(bad);
            assert_eq!(
                s.active().unwrap().state,
                State::Working,
                "invalid id must not overwrite root"
            );
        }
        for bad in [
            format!("state=done:msg={}:msg=", "a".repeat(2733)),
            format!("state=done:app={}:app=ok", "a".repeat(33)),
            format!("state=done:{}=a", "k".repeat(17)),
            format!("state=done:id={}", "a/".repeat(9)),
            format!("state=done:unused={}", "a".repeat(4096)),
        ] {
            s.report(bad.as_bytes());
            assert_eq!(s.active().unwrap().state, State::Working);
        }
        s.report(b" state = done :unknown=ok:bad$:state=idle:msg=SGVsbG8:kind=question:progress=9");
        let r = s.active().unwrap();
        assert_eq!(r.state, State::Idle);
        assert_eq!(r.msg.as_deref(), Some("Hello"));
        assert_eq!((r.kind, r.progress), (None, None));
        s.report(b"broken=\xff:state=done");
        assert_eq!(
            s.active().unwrap().state,
            State::Done,
            "malformed pairs are skipped independently"
        );
    }

    #[test]
    fn records_are_bounded_and_updates_refresh_eviction_order() {
        let mut s = ProgramStatus::default();
        for i in 0..256 {
            s.report(format!("state=idle:id={i}").as_bytes());
        }
        s.report(b"state=blocked:id=0");
        s.report(b"state=done:id=256");
        assert_eq!(s.records.len(), 256);
        assert!(!s.records.iter().any(|r| r.id == "1"));
        assert_eq!(s.active().unwrap().id, "0");
        s.end_command();
        assert_eq!(s.active().unwrap().id, "256");
        s.reset();
        assert!(s.active().is_none());
    }
}

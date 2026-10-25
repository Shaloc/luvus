//! Display-side Kitty paste events (OSC 5522). Kitty consumes Cmd+V itself;
//! requesting paste events lets its normal shortcut also deliver image offers.
//! Crossterm remains the input reader: it exposes unrecognized OSC as Alt+]
//! followed by character events. Reassemble only this bounded host protocol,
//! before application input dispatch. Image data still uses the official kitten.

use std::collections::VecDeque;
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use super::{capture_with_input, decode_base64, kitten, ClipboardImage};

const MAX_OFFER: usize = 16 * 1024;
static ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) enum Input {
    Event(Event),
    Image(ClipboardImage),
}

pub(crate) fn enable() {
    // Without a reader, retain the terminal's ordinary text-paste behavior.
    // Old Kitty versions ignore this mode and continue bracketed text paste.
    if crate::terminal::is_kitty_display() && kitten::executable().is_some() {
        ENABLED.store(true, Ordering::Relaxed);
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[?5522h");
        let _ = out.flush();
    }
}

pub(crate) fn disable() {
    if ENABLED.swap(false, Ordering::Relaxed) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[?5522l");
        let _ = out.flush();
    }
}

pub(crate) fn run_input_loop(pending: Vec<Event>, mut send: impl FnMut(Input) -> bool) {
    for event in pending {
        if !send(Input::Event(event)) {
            return;
        }
    }
    let mut offer = None;
    while let Ok(event) = event::read() {
        if ENABLED.load(Ordering::Relaxed) && alt(&event, ']') {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut pending = VecDeque::new();
            let mut read = |timeout| {
                let remaining = deadline.checked_duration_since(Instant::now())?;
                event::poll(remaining.min(timeout))
                    .ok()
                    .filter(|ready| *ready)
                    .and_then(|_| event::read().ok())
            };
            match read_osc(event, &mut read, &mut pending) {
                Some(frame) => {
                    if let Some(ready) = accept_frame(&mut offer, &frame) {
                        if let Some(input) = read_offer(ready) {
                            if !send(input) {
                                break;
                            }
                        }
                    }
                }
                None => offer = None,
            }
            for event in pending {
                if !send(Input::Event(event)) {
                    return;
                }
            }
        } else if !send(Input::Event(event)) {
            break;
        }
    }
}

fn alt(event: &Event, character: char) -> bool {
    matches!(event, Event::Key(key) if key.code == KeyCode::Char(character)
        && key.modifiers == KeyModifiers::ALT)
}

fn ascii(event: &Event) -> Option<char> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers,
            ..
        }) if c.is_ascii()
            && !c.is_ascii_control()
            && (*modifiers == KeyModifiers::NONE || *modifiers == KeyModifiers::SHIFT) =>
        {
            Some(*c)
        }
        _ => None,
    }
}

/// Preserve a real Alt+] binding and nonmatching input byte-for-byte as Events.
/// Once the exact OSC number is recognized, malformed/oversized frames are
/// consumed, not typed into a shell. The caller bounds the entire read in time.
fn read_osc(
    first: Event,
    read: &mut impl FnMut(Duration) -> Option<Event>,
    pending: &mut VecDeque<Event>,
) -> Option<String> {
    let mut prefix = vec![first];
    for (index, expected) in "5522;".chars().enumerate() {
        // A bare Alt+] should remain responsive. Once the protocol prefix has
        // started, use the caller's whole-frame deadline: SSH can split the
        // prefix itself across packets, not just the metadata after it.
        let event = read(if index == 0 {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        });
        let matches = event.as_ref().and_then(ascii) == Some(expected);
        prefix.extend(event);
        if !matches {
            // These have already been examined. Do not parse Alt+] again.
            pending.extend(prefix);
            return None;
        }
    }
    let mut frame = String::new();
    let mut oversized = false;
    while let Some(event) = read(Duration::from_secs(5)) {
        if alt(&event, '\\')
            || matches!(&event, Event::Key(k)
            if k.code == KeyCode::Char('g') && k.modifiers == KeyModifiers::CONTROL)
        {
            return (!oversized).then_some(frame);
        }
        if let Some(c) = ascii(&event) {
            if frame.len() < MAX_OFFER {
                frame.push(c);
            } else {
                oversized = true;
            }
        } else {
            // Resize/focus/mouse input can interleave protocol fragments.
            pending.push_back(event);
        }
    }
    None
}

struct Offer {
    password: String,
    primary: bool,
    mimes: String,
    started: Instant,
}

fn accept_frame(offer: &mut Option<Offer>, frame: &str) -> Option<Offer> {
    let (metadata, payload) = frame.split_once(';').unwrap_or((frame, ""));
    let field = |name| {
        metadata.split(':').find_map(|part| {
            let (key, value) = part.split_once('=')?;
            (key == name).then_some(value)
        })
    };
    if field("type") != Some("read") || field("id").is_some() {
        *offer = None;
        return None;
    }
    let password = field("pw")
        .and_then(|pw| decode_base64(pw, 256).ok())
        .and_then(|pw| String::from_utf8(pw).ok())
        .filter(|pw| !pw.is_empty() && pw.bytes().all(|c| c.is_ascii_graphic()));
    let Some(password) = password else {
        *offer = None;
        return None;
    };
    if field("status") == Some("OK") {
        *offer = Some(Offer {
            password,
            primary: field("loc") == Some("primary"),
            mimes: String::new(),
            started: Instant::now(),
        });
        return None;
    }
    let active = offer.as_mut()?;
    if active.password != password || active.started.elapsed() > Duration::from_secs(15) {
        *offer = None;
        return None;
    }
    match field("status") {
        Some("DATA") if field("mime") == Some("Lg==") => {
            let mimes = decode_base64(payload, MAX_OFFER)
                .ok()
                .and_then(|data| String::from_utf8(data).ok());
            if let Some(mimes) = mimes.filter(|m| m.len() + active.mimes.len() <= MAX_OFFER) {
                active.mimes.push_str(&mimes);
            } else {
                *offer = None;
            }
            None
        }
        Some("DONE") => offer.take(),
        _ => {
            *offer = None;
            None
        }
    }
}

fn preferred_mime(mimes: &str) -> Option<(&'static str, &'static str)> {
    super::IMAGE_MIMES
        .into_iter()
        .chain([("text/plain", "")])
        .find(|(mime, _)| mimes.split_whitespace().any(|m| m == *mime))
}

fn read_offer(offer: Offer) -> Option<Input> {
    let (mime, extension) = preferred_mime(&offer.mimes)?;
    let mut command = Command::new(kitten::executable()?);
    command.args([
        "clipboard",
        "--get-clipboard",
        "--mime",
        mime,
        "--password",
        "fd:0",
        "--human-name",
        "Paste event",
    ]);
    if offer.primary {
        command.arg("--use-primary");
    }
    command.arg("/dev/stdout");
    // The offer grants this one explicit paste; no permission popup or ambient
    // clipboard access is needed. Wait for DONE before giving kitten the TTY.
    let bytes = capture_with_input(
        &mut command,
        Duration::from_secs(30),
        Some(offer.password.as_bytes()),
    )?;
    if extension.is_empty() {
        if bytes.is_empty() {
            return None;
        }
        Some(Input::Event(Event::Paste(paste_text(&bytes))))
    } else {
        let image = ClipboardImage {
            extension: extension.into(),
            bytes,
        };
        image.valid().then_some(Input::Image(image))
    }
}

fn paste_text(bytes: &[u8]) -> String {
    // Paste events bypass Kitty's normal paste_actions filtering. Keep text,
    // tabs and newlines, but render control codes visibly so a clipboard ESC
    // cannot terminate the child's bracketed paste or become a shell binding.
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + c as u32).unwrap(),
            '\u{7f}' => '\u{2421}',
            '\u{80}'..='\u{9f}' => '\u{fffd}',
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn characters(text: &str) -> impl Iterator<Item = Event> + '_ {
        text.chars().map(|c| {
            Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                if c.is_ascii_uppercase() {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::NONE
                },
            ))
        })
    }

    fn opener() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT))
    }
    fn terminator() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT))
    }

    #[test]
    fn paste_events_reassemble_only_the_exact_protocol_and_preserve_other_input() {
        let mut events = characters("5522;type=read:status=OK:pw=dGVzdA==")
            .chain([Event::Resize(80, 24), terminator()]);
        let mut pending = VecDeque::new();
        assert_eq!(
            read_osc(opener(), &mut |_| events.next(), &mut pending).as_deref(),
            Some("type=read:status=OK:pw=dGVzdA==")
        );
        assert_eq!(pending, [Event::Resize(80, 24)]);
        for text in ["", "x", "55x", "5523;"] {
            let mut events = characters(text);
            let mut pending = VecDeque::new();
            assert!(read_osc(opener(), &mut |_| events.next(), &mut pending).is_none());
            assert_eq!(
                pending.into_iter().chain(events).collect::<Vec<_>>(),
                std::iter::once(opener())
                    .chain(characters(text))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn paste_events_preserve_offers_split_inside_the_protocol_prefix() {
        for split in 1..5 {
            let mut events =
                characters("5522;type=read:status=OK:pw=dGVzdA==").chain([terminator()]);
            let mut index = 0;
            let mut pending = VecDeque::new();
            let frame = read_osc(
                opener(),
                &mut |timeout| {
                    if index == split && timeout < Duration::from_millis(200) {
                        return None;
                    }
                    index += 1;
                    events.next()
                },
                &mut pending,
            );
            assert_eq!(
                frame.as_deref(),
                Some("type=read:status=OK:pw=dGVzdA=="),
                "fragment boundary after {split} prefix characters"
            );
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn paste_events_discard_truncated_and_oversized_protocol_without_typing_it() {
        for text in [
            "5522;type=read:status=".into(),
            format!("5522;{}", "x".repeat(MAX_OFFER + 1)),
        ] {
            let mut events = characters(&text);
            let mut pending = VecDeque::new();
            assert!(read_osc(opener(), &mut |_| events.next(), &mut pending).is_none());
            assert!(pending.is_empty());
        }
        let text = format!("5522;{}", "x".repeat(MAX_OFFER + 1));
        let mut events = characters(&text).chain([terminator()]);
        assert!(read_osc(opener(), &mut |_| events.next(), &mut VecDeque::new()).is_none());
    }

    #[test]
    fn paste_events_wait_for_done_and_match_the_one_use_password() {
        let mut offer = None;
        assert!(accept_frame(&mut offer, "type=read:status=OK:loc=primary:pw=dGVzdA==").is_none());
        assert!(accept_frame(
            &mut offer,
            "type=read:status=DATA:pw=dGVzdA==:mime=Lg==;aW1hZ2UvcG5nIHRleHQvcGxhaW4K"
        )
        .is_none());
        let ready = accept_frame(&mut offer, "type=read:status=DONE:pw=dGVzdA==").unwrap();
        assert!(ready.primary);
        assert_eq!(ready.password, "test");
        assert_eq!(preferred_mime(&ready.mimes), Some(("image/png", "png")));
        assert!(offer.is_none());
        assert!(accept_frame(&mut offer, "type=read:status=DONE:pw=dGVzdA==").is_none());
        accept_frame(&mut offer, "type=read:status=OK:pw=dGVzdA==");
        assert!(accept_frame(&mut offer, "type=read:status=DONE:pw=b3RoZXI=").is_none());
        assert!(offer.is_none());
    }

    #[test]
    fn paste_events_reject_responses_without_a_valid_offer_and_expired_offers() {
        for frame in [
            "type=read:status=OK",
            "type=read:status=OK:pw=!!!=",
            "type=read:status=OK:pw=dGVzdA",
            "type=read:status=OK:pw=dGVzdA==:id=external",
        ] {
            let mut offer = None;
            accept_frame(&mut offer, frame);
            assert!(offer.is_none(), "{frame}");
        }
        let mut offer = None;
        accept_frame(&mut offer, "type=read:status=OK:pw=dGVzdA==");
        offer.as_mut().unwrap().started -= Duration::from_secs(16);
        assert!(accept_frame(&mut offer, "type=read:status=DONE:pw=dGVzdA==").is_none());
        assert!(offer.is_none());
        assert_eq!(
            preferred_mime("text/html text/plain\n"),
            Some(("text/plain", ""))
        );
        assert_eq!(preferred_mime("application/octet-stream"), None);
    }

    #[test]
    fn paste_events_text_preserves_unicode_and_lines_without_injecting_controls() {
        assert_eq!(
            paste_text("中文\tfirst\r\nsecond\rthird\n".as_bytes()),
            "中文\tfirst\nsecond\nthird\n"
        );
        assert_eq!(paste_text(b"\x1b[201~\x03test\x00\x7f"), "␛[201~␃test␀␡");
        assert!(!paste_text("\u{9b}201~".as_bytes()).contains('\u{9b}'));
    }
}

use std::io::{BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde_json::json;

use super::protocol::{self as wire, ClientMessage, ServerMessage};

mod browser;
pub mod protocol;
pub mod schema;

type Output = Arc<Mutex<std::io::Stdout>>;

fn emit(output: &Output, response: &protocol::Response) -> std::io::Result<()> {
    let mut output = output
        .lock()
        .map_err(|_| std::io::Error::other("app output closed"))?;
    protocol::write(&mut *output, response)
}

pub fn requested(args: &[String]) -> bool {
    args.get(1).is_some_and(|arg| arg == "client")
        && args.iter().skip(2).any(|arg| arg == "--surface")
}

pub fn run(args: &[String]) -> Result<()> {
    let options = Options::parse(&args[2..])?;
    if options.schema {
        println!("{}", serde_json::to_string_pretty(&schema::document())?);
        return Ok(());
    }
    crate::ensure_interactive_launch_allowed()?;
    if let Some(host) = options.remote {
        let mut remote_args = vec![args[0].clone(), "--remote".into(), host];
        remote_args.extend(options.ssh_args);
        return crate::remote_attach_surface(&remote_args, true);
    }
    if let Some(target) = crate::session::remote::process_target() {
        crate::prepare_managed_remote_attach(&target)?;
    }
    let stream = super::transport::connect(&crate::persist::client_socket_path())
        .map_err(|_| anyhow!("cannot connect to the selected Luvus server"))?;
    attach(stream.clone(), stream, Ok(None))
}

#[derive(Debug, PartialEq)]
struct Options {
    schema: bool,
    remote: Option<String>,
    ssh_args: Vec<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut surface = false;
        let mut stdio = false;
        let mut options = Self {
            schema: false,
            remote: None,
            ssh_args: Vec::new(),
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--surface" if !surface => {
                    if args.next().map(String::as_str) != Some("app") {
                        return Err(anyhow!("expected --surface app"));
                    }
                    surface = true;
                }
                "--stdio" if !stdio => stdio = true,
                "--schema" if !options.schema => options.schema = true,
                "--remote" if options.remote.is_none() => {
                    let host = args
                        .next()
                        .ok_or_else(|| anyhow!("--remote requires a host"))?;
                    crate::session::remote::validate_host_alias(host)
                        .map_err(anyhow::Error::msg)?;
                    options.remote = Some(host.clone());
                }
                "--" if options.remote.is_some() => {
                    options.ssh_args.extend(args.cloned());
                    break;
                }
                _ => return Err(anyhow!("invalid app client argument")),
            }
        }
        if !surface || stdio == options.schema || options.schema && options.remote.is_some() {
            return Err(anyhow!("use client --surface app --stdio or --schema"));
        }
        Ok(options)
    }
}

pub fn attach<R: Read + Send + 'static, W: Write + Send + 'static>(
    reader: R,
    mut writer: W,
    browser_route: Result<Option<String>, String>,
) -> Result<()> {
    super::client::write_handshake_message(
        &mut writer,
        &ClientMessage::HelloApp {
            version: protocol::TRANSPORT_VERSION,
            cols: 120,
            rows: 40,
        },
    )?;
    let mut reader = BufReader::new(reader);
    match super::client::read_welcome_message(&mut reader)? {
        ServerMessage::Welcome {
            error: Some(error), ..
        } => {
            return Err(anyhow!(
                "{error}; the selected server must support the App surface; it was not restarted"
            ))
        }
        ServerMessage::Welcome {
            version,
            error: None,
        } if version >= protocol::TRANSPORT_VERSION => {}
        _ => {
            return Err(anyhow!(
                "the selected server does not support the App surface"
            ))
        }
    }
    match super::client::read_handshake_message(&mut reader)? {
        ServerMessage::Ready { probe_terminal } => {
            if probe_terminal {
                super::client::write_handshake_message(
                    &mut writer,
                    &ClientMessage::TerminalColors(None),
                )?;
            }
        }
        _ => return Err(anyhow!("invalid App handshake")),
    }
    let writer = Arc::new(Mutex::new(writer));
    let output = Arc::new(Mutex::new(std::io::stdout()));
    let input_output = output.clone();
    let input_writer = writer.clone();
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        while let Ok(Some(request)) = protocol::read(&mut input) {
            // A rejected action has a complete frame. Keep the attachment and its PTYs intact.
            if let Err(message) = request.validate() {
                let id = request.id();
                if !request.has_valid_id() {
                    break;
                }
                if emit(
                    &input_output,
                    &protocol::Response::Error {
                        id: Some(id.into()),
                        code: "invalid_request".into(),
                        message: message.into(),
                    },
                )
                .is_err()
                {
                    break;
                }
                continue;
            }
            let detach = matches!(request, protocol::Request::Detach { .. });
            let Ok(body) = serde_json::to_string(&request) else {
                break;
            };
            let Ok(mut writer) = input_writer.lock() else {
                break;
            };
            if wire::write_message(&mut *writer, &ClientMessage::AppRequest(body)).is_err()
                || detach
            {
                break;
            }
        }
        if let Ok(mut writer) = input_writer.lock() {
            let _ = wire::write_message(&mut *writer, &ClientMessage::Detach);
        }
    });
    let mut browsers = browser::Sessions::default();
    emit(
        &output,
        &protocol::Response::Ready {
            protocol: protocol::VERSION,
            transport: protocol::TRANSPORT_VERSION,
            capabilities: vec![
                "app_projection.v1".into(),
                "terminal_cells.v1".into(),
                "terminal_graphics.v1".into(),
            ],
        },
    )?;
    loop {
        let message = match wire::read_message(&mut reader) {
            Ok(message) => message,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        };
        let response = match message {
            ServerMessage::AppFrame(body) => {
                let messages: Vec<protocol::Response> = serde_json::from_str(&body)?;
                for response in messages {
                    forward_response(response, &mut browsers, &browser_route, &output)?;
                }
                continue;
            }
            ServerMessage::AppResponse(body) => serde_json::from_str(&body)?,
            ServerMessage::Clipboard(text) | ServerMessage::ScopedClipboard { text, .. } => {
                effect("clipboard", json!({"text":text}))
            }
            ServerMessage::ClipboardTracked { text, receipt } => effect(
                "clipboard",
                json!({"text":text,"receipt":receipt.to_string()}),
            ),
            ServerMessage::Notify(text) => effect("notification", json!({"text":text})),
            ServerMessage::Notification { text, level } => {
                effect("notice", json!({"text":text,"level":level}))
            }
            ServerMessage::Sound(sound) => effect("sound", serde_json::to_value(sound)?),
            ServerMessage::OpenUrl(url) => effect("open_url", json!({"url":url})),
            ServerMessage::SwitchSession { name } => {
                emit(&output, &effect("switch_session", json!({"name":name})))?;
                break;
            }
            ServerMessage::ServerShutdown { reason } => {
                emit(&output, &effect("disconnected", json!({"reason":reason})))?;
                break;
            }
            ServerMessage::Detach => break,
            ServerMessage::Browser(_) | ServerMessage::BrowserWithProxy(_) => {
                protocol::Response::Error {
                    id: None,
                    code: "unavailable".into(),
                    message: "embedded browser networking is not attached".into(),
                }
            }
            _ => continue,
        };
        forward_response(response, &mut browsers, &browser_route, &output)?;
    }
    drop(browsers);
    Ok(())
}

fn forward_response(
    response: protocol::Response,
    browsers: &mut browser::Sessions,
    route: &Result<Option<String>, String>,
    output: &Output,
) -> Result<()> {
    match &response {
        protocol::Response::Snapshot { snapshot } => browsers.retain_snapshot(snapshot),
        protocol::Response::Effect { kind, data } if kind == "browser_launch" => {
            let failure = effect(
                "browser_error",
                json!({
                    "request_id":data["request_id"],"browser_id":data["browser_id"],
                    "target":data["target"],"lease_generation":data["lease_generation"],"code":"display_failed",
                }),
            );
            if browsers.open(data.clone(), route, output.clone()).is_err() {
                emit(output, &failure)?;
            }
            return Ok(());
        }
        protocol::Response::Effect { kind, data } if kind == "browser_command" => {
            if data["method"] == "browser.close" && data["params"]["reason"].is_string()
                || data["method"] == "browser.control.release"
                    && data["params"]["reason"] == "browser_closed"
            {
                if let Some(id) = data["browser_id"].as_str() {
                    browsers.close(id);
                }
            }
        }
        protocol::Response::Effect { kind, .. }
            if matches!(
                kind.as_str(),
                "browser_open" | "browser_closed" | "browser_error" | "browser_status"
            ) =>
        {
            return Err(anyhow!(
                "owner attempted to send a display-only browser effect"
            ));
        }
        _ => {}
    }
    emit(output, &response)?;
    Ok(())
}

fn effect(kind: &str, data: serde_json::Value) -> protocol::Response {
    protocol::Response::Effect {
        kind: kind.into(),
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn app_surface_options_are_explicit() {
        assert!(Options::parse(&args(&["--surface", "app", "--stdio"])).is_ok());
        assert!(Options::parse(&args(&["--surface", "app", "--schema"])).is_ok());
        for options in [
            vec!["--stdio"],
            vec!["--surface", "app"],
            vec!["--surface", "app", "--stdio", "--schema"],
            vec!["--surface", "other", "--stdio"],
        ] {
            assert!(Options::parse(&args(&options)).is_err());
        }
    }
}

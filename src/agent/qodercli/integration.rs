use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use super::super::types::IntegrationOperations;
use crate::integration;

pub(super) const OPERATIONS: IntegrationOperations = IntegrationOperations {
    install,
    uninstall,
    is_installed,
    hook: Some(run_hook),
};

const EVENT: &str = "SessionStart";
const HOOK_TIMEOUT_SECONDS: u64 = 10;
const MAX_HOOK_PAYLOAD: u64 = 1024 * 1024;

#[cfg(windows)]
const SCRIPT_NAME: &str = "luvus-agent-hook.ps1";
#[cfg(not(windows))]
const SCRIPT_NAME: &str = "luvus-agent-hook.sh";

#[cfg(any(not(windows), test))]
const SHELL_SCRIPT: &str = include_str!("hook.sh");
#[cfg(any(windows, test))]
const POWERSHELL_SCRIPT: &str = include_str!("hook.ps1");

#[cfg(windows)]
const SCRIPT: &str = POWERSHELL_SCRIPT;
#[cfg(not(windows))]
const SCRIPT: &str = SHELL_SCRIPT;

fn config_dir() -> PathBuf {
    std::env::var_os("QODER_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| integration::home().join(".qoder"))
}

fn config_path() -> PathBuf {
    config_dir().join("settings.json")
}

fn script_path() -> PathBuf {
    config_dir().join("hooks").join(SCRIPT_NAME)
}

#[cfg(not(windows))]
fn hook_command(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("Qoder CLI integration path is not valid Unicode"))?;
    Ok(format!("sh '{}' session", path.replace('\'', "'\\''")))
}

#[cfg(windows)]
fn hook_command(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("Qoder CLI integration path is not valid Unicode"))?;
    Ok(format!(
        "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\" session",
        path.replace('"', "\"\"")
    ))
}

fn managed_group(command: &str) -> Value {
    json!({
        "matcher": "*",
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": HOOK_TIMEOUT_SECONDS,
        }],
    })
}

fn group_references_command(group: &Value, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .any(|hook| hook.get("command").and_then(Value::as_str) == Some(command))
        })
}

fn read_config(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).map_err(Into::into),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(error.into()),
    }
}

fn session_start_groups(value: &mut Value) -> Result<&mut Vec<Value>> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("Qoder CLI settings must contain a JSON object"))?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("Qoder CLI settings `hooks` must contain a JSON object"))?;
    hooks
        .entry(EVENT)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("Qoder CLI settings `hooks.{EVENT}` must contain an array"))
}

fn install() -> Result<()> {
    let root = config_dir();
    if !root.is_dir() {
        return Err(anyhow!(
            "Qoder CLI config directory not found at {}. Install and run Qoder CLI first.",
            root.display()
        ));
    }

    let config = config_path();
    let script = script_path();
    let command = hook_command(&script)?;
    let expected = managed_group(&command);
    let mut value = read_config(&config)?;
    let groups = session_start_groups(&mut value)?;
    if groups
        .iter()
        .any(|group| group_references_command(group, &command) && group != &expected)
    {
        return Err(anyhow!(
            "Qoder CLI settings contain a modified Luvus hook; preserve it or remove it explicitly"
        ));
    }
    groups.retain(|group| group != &expected);
    groups.push(expected);

    if script.is_file() && fs::read_to_string(&script).ok().as_deref() != Some(SCRIPT) {
        return Err(anyhow!(
            "Qoder CLI hook path already contains an unmanaged file: {}",
            script.display()
        ));
    }
    fs::create_dir_all(script.parent().expect("hook path has a parent"))?;
    let script_was_present = script.is_file();
    if !script_was_present {
        integration::write_bytes_atomic(&script, SCRIPT.as_bytes())?;
    }
    integration::set_executable(&script)?;
    if let Err(error) = integration::write_json_atomic(&config, &value) {
        if !script_was_present {
            let _ = fs::remove_file(&script);
        }
        return Err(error);
    }
    Ok(())
}

fn uninstall() -> Result<()> {
    let config = config_path();
    let script = script_path();
    let command = hook_command(&script)?;
    let expected = managed_group(&command);
    let mut still_referenced = false;

    if config.is_file() {
        let mut value = read_config(&config)?;
        let groups = session_start_groups(&mut value)?;
        let before = groups.len();
        groups.retain(|group| group != &expected);
        still_referenced = groups
            .iter()
            .any(|group| group_references_command(group, &command));
        if groups.len() != before {
            integration::write_json_atomic(&config, &value)?;
        }
    }

    if !still_referenced && fs::read_to_string(&script).ok().as_deref() == Some(SCRIPT) {
        let _ = fs::remove_file(script);
    }
    Ok(())
}

fn is_installed() -> bool {
    let script = script_path();
    if fs::read_to_string(&script).ok().as_deref() != Some(SCRIPT) {
        return false;
    }
    let Ok(command) = hook_command(&script) else {
        return false;
    };
    let expected = managed_group(&command);
    read_config(&config_path())
        .ok()
        .and_then(|mut value| session_start_groups(&mut value).ok().cloned())
        .is_some_and(|groups| groups.iter().any(|group| group == &expected))
}

fn hook_params(input: &[u8], pane: &str) -> Option<Value> {
    if input.len() as u64 > MAX_HOOK_PAYLOAD || pane.is_empty() {
        return None;
    }
    let payload: Value = serde_json::from_slice(input).ok()?;
    let session = payload.get("session_id")?.as_str()?;
    crate::agent::resume_command(super::NAME, session)?;
    Some(json!({
        "pane": pane,
        "agent": super::NAME,
        "session_id": session,
    }))
}

fn read_hook_input_from(reader: &mut impl Read) -> Option<Vec<u8>> {
    let mut input = Vec::new();
    let mut limited = reader.take(MAX_HOOK_PAYLOAD + 1);
    limited.read_to_end(&mut input).ok()?;
    (input.len() as u64 <= MAX_HOOK_PAYLOAD).then_some(input)
}

fn run_hook() -> i32 {
    let environment_matches = std::env::var_os("LUVUS_ENV").as_deref()
        == Some(std::ffi::OsStr::new("1"))
        && std::env::var_os("LUVUS_SOCKET_PATH").is_some_and(|path| !path.is_empty());
    if environment_matches {
        let stdin = io::stdin();
        if let (Some(input), Ok(pane)) = (
            read_hook_input_from(&mut stdin.lock()),
            std::env::var("LUVUS_PANE_ID"),
        ) {
            if let Some(params) = hook_params(&input, &pane) {
                let _ = crate::cli::send_request("pane.report_session", params);
            }
        }
    }
    println!("{{}}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "luvus-qodercli-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn install_is_idempotent_and_uninstall_preserves_user_settings() {
        let _env = crate::persist::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = temp_root("install");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("QODER_CONFIG_DIR", &root);
        fs::write(
            root.join("settings.json"),
            r#"{"model":"keep","hooks":{"SessionStart":[{"matcher":"*","hooks":[{"type":"command","command":"echo mine"}]}]}}"#,
        )
        .unwrap();

        install().unwrap();
        install().unwrap();
        assert!(is_installed());
        let value = read_config(&root.join("settings.json")).unwrap();
        assert_eq!(value["model"], "keep");
        let groups = value["hooks"][EVENT].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups
            .iter()
            .any(|group| { group["hooks"][0]["command"].as_str() == Some("echo mine") }));
        assert_eq!(
            groups
                .iter()
                .filter(|group| group["hooks"][0]["timeout"] == HOOK_TIMEOUT_SECONDS)
                .count(),
            1
        );
        assert!(SHELL_SCRIPT.contains("integration hook qodercli"));
        assert!(POWERSHELL_SCRIPT.contains("integration hook qodercli"));

        uninstall().unwrap();
        assert!(!is_installed());
        assert!(!script_path().exists());
        let value = read_config(&root.join("settings.json")).unwrap();
        assert_eq!(value["model"], "keep");
        let groups = value["hooks"][EVENT].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["hooks"][0]["command"], "echo mine");

        std::env::remove_var("QODER_CONFIG_DIR");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_settings_and_missing_install_are_preserved() {
        let _env = crate::persist::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = temp_root("invalid");
        let _ = fs::remove_dir_all(&root);
        std::env::set_var("QODER_CONFIG_DIR", &root);
        assert!(install().is_err(), "Qoder must already own its config dir");

        fs::create_dir_all(&root).unwrap();
        for invalid in [
            "{ user settings",
            r#"{"hooks":[]}"#,
            r#"{"hooks":{"SessionStart":{}}}"#,
        ] {
            fs::write(root.join("settings.json"), invalid).unwrap();
            assert!(install().is_err());
            assert_eq!(
                fs::read_to_string(root.join("settings.json")).unwrap(),
                invalid
            );
            assert!(!script_path().exists());
        }

        std::env::remove_var("QODER_CONFIG_DIR");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hook_accepts_only_bounded_safe_session_ids() {
        let params = hook_params(
            br#"{"session_id":"ec33ebf9-0cba-4100-8142-c61503f6c587","transcript":"private"}"#,
            "42",
        )
        .unwrap();
        assert_eq!(params["pane"], "42");
        assert_eq!(params["agent"], super::super::NAME);
        assert_eq!(params["session_id"], "ec33ebf9-0cba-4100-8142-c61503f6c587");
        assert!(params.get("transcript").is_none());
        assert!(hook_params(br#"{"session_id":"bad id"}"#, "42").is_none());
        assert!(hook_params(&vec![b'x'; MAX_HOOK_PAYLOAD as usize + 1], "42").is_none());
    }
}

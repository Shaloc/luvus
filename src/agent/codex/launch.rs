//! Captured launch arguments must never retain a previous pane's hook route.

fn owned_hook_override(value: &str) -> bool {
    let Some((key, value)) = value.split_once('=') else {
        return false;
    };
    matches!(key, "hooks.SessionStart" | "hooks.UserPromptSubmit")
        && value.contains("LUVUS_CODEX_HOOK_CONTEXT=1")
        && value.contains("luvus-agent-hook.sh")
}

pub(in crate::agent) fn persistent_flags(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if matches!(arg.as_str(), "-c" | "--config")
            && args.get(i + 1).is_some_and(|v| owned_hook_override(v))
        {
            i += 2;
            continue;
        }
        if arg
            .strip_prefix("--config=")
            .or_else(|| arg.strip_prefix("-c"))
            .is_some_and(|v| owned_hook_override(v.trim_start_matches('=')))
        {
            i += 1;
            continue;
        }
        out.push(arg.clone());
        i += 1;
    }
    out
}

pub(in crate::agent) fn resume_flags(args: &[String]) -> Vec<String> {
    let args = persistent_flags(args);
    let mut out = Vec::new();
    let mut i = 0;
    let mut command_seen = false;
    let mut session_pending = false;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            // The remainder is an initial prompt, not reusable launch options.
            break;
        }
        if matches!(
            arg,
            "-c" | "--config"
                | "-m"
                | "--model"
                | "-p"
                | "--profile"
                | "-s"
                | "--sandbox"
                | "-a"
                | "--ask-for-approval"
                | "-C"
                | "--cd"
                | "--add-dir"
                | "-i"
                | "--image"
                | "--enable"
                | "--disable"
                | "--local-provider"
                | "--remote-auth-token-env"
        ) {
            out.push(args[i].clone());
            i += 1;
            if let Some(value) = args.get(i) {
                out.push(value.clone());
                i += 1;
            }
            continue;
        }
        if !command_seen && matches!(arg, "resume" | "fork") {
            command_seen = true;
            session_pending = true;
            i += 1;
            continue;
        }
        if session_pending && matches!(arg, "--last" | "--all") {
            i += 1;
            continue;
        }
        if !arg.starts_with('-') {
            command_seen = true;
            if session_pending {
                session_pending = false;
                i += 1;
                continue;
            }
        }
        out.push(args[i].clone());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_flags_remove_only_owned_routes_including_old_snapshots() {
        // Process command tokenization may already have removed TOML quotes.
        let route = "hooks.UserPromptSubmit=[{hooks=[{command=env LUVUS_CODEX_HOOK_CONTEXT=1 /old/luvus-agent-hook.sh}]}]";
        let user = "hooks.SessionStart=[{hooks=[{command=echo user}]}]";
        for prefix in ["-c", "--config"] {
            let args = [prefix, route, "-c", user, "--model", "resume"].map(str::to_owned);
            assert_eq!(persistent_flags(&args), ["-c", user, "--model", "resume"]);
        }
        for prefix in ["-c", "-c=", "--config="] {
            assert!(persistent_flags(&[format!("{prefix}{route}")]).is_empty());
        }
        assert_eq!(
            resume_flags(&[
                "--config=model=resume".into(),
                "resume".into(),
                "--last".into(),
                "--all".into()
            ]),
            ["--config=model=resume"]
        );
    }
}

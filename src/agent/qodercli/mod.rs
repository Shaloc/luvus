//! Native Qoder CLI identity, launch, resume, and optional session integration.
//!
//! Qoder's SessionStart hook reports the exact session id. Luvus deliberately
//! does not read Qoder's private transcript store; without the integration,
//! process and screen detection still provide live sidebar state.

use super::types::{AgentDescriptor, IdentityDescriptor, SessionOperations};

mod integration;

pub(crate) const NAME: &str = "qodercli";

pub(super) const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    id: NAME,
    aliases: &["qoderclicn", "qoder", "qodercn"],
    launch_command: "qodercli",
    task_prompt_args: &["--prompt-interactive"],
    // Interactive prompts are supported; scheduled access policies have not
    // been reviewed for Qoder's one-shot entrypoint.
    automation: None,
    identity: IdentityDescriptor {
        distinct: &["qodercli", "qoderclicn", "qodercn"],
        // `qoder` is also used in prose and package paths. Trust it only where
        // the detector has deliberate command/title evidence.
        ambiguous: &["qoder"],
        binary_matcher: Some(is_versioned_binary),
        interpreter_packages: &[],
        overlap_priority: 0,
    },
    sessions: Some(SessionOperations {
        // Qoder has a hook contract for exact ownership, but Luvus does not
        // inspect its private JSONL transcripts merely to discover sessions.
        discovery: None,
        resume: |session| format!("qodercli --resume {session}\r"),
        // Although recent CLIs expose an interactive fork flag, there is no
        // reviewed external stored-session fork contract comparable to resume.
        fork: None,
    }),
    integration: Some(integration::OPERATIONS),
};

/// Qoder's launcher execs a release-specific binary such as
/// `qodercli-1.1.42`. A leading digit keeps helper names from matching.
pub(super) fn is_versioned_binary(binary: &str) -> bool {
    binary
        .strip_prefix("qodercli-")
        .and_then(|version| version.as_bytes().first())
        .is_some_and(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_binary_matcher_rejects_helpers() {
        assert!(is_versioned_binary("qodercli-1.1.42"));
        assert!(!is_versioned_binary("qodercli-helper"));
        assert!(!is_versioned_binary("qodercli-"));
    }

    #[test]
    fn task_prompts_use_qoders_interactive_prompt_flag() {
        assert_eq!(DESCRIPTOR.launch_command, "qodercli");
        assert_eq!(DESCRIPTOR.task_prompt_args, ["--prompt-interactive"]);
    }
}

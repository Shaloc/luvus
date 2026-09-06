//! Optional official clipboard helper, installed only by an explicit display-
//! client action. Reuses Luvus's downloader and private-file/atomic-replace
//! boundaries. Never changes PATH, the user's existing kitten, or Kitty grants.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub generation: String,
    pub install: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Missing,
    Installed(String),
    Unsupported,
}

pub type Outcome = Result<Status, String>;

fn managed_path() -> PathBuf {
    crate::persist::config_dir().join("tools").join("kitten")
}

pub fn executable() -> Option<PathBuf> {
    let managed = managed_path();
    std::iter::once(managed)
        .chain(std::env::var_os("PATH").into_iter().flat_map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("kitten"))
                .collect::<Vec<_>>()
        }))
        .find(|path| {
            let Ok(meta) = std::fs::metadata(path) else {
                return false;
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                meta.is_file() && meta.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                meta.is_file()
            }
        })
}

fn asset() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some((
            "kitten-linux-amd64",
            "29f1fc2353ffcc880b90bc4a4199f187caef1b608da7808e4a5f46404e875991",
        )),
        ("linux", "aarch64") => Some((
            "kitten-linux-arm64",
            "2c40353d17728d4cfdc019ebcd8fe61e718b7b765b765c981e82f86bd89d535a",
        )),
        ("macos", "x86_64") => Some((
            "kitten-darwin-amd64",
            "0f846d8c7caa5313827253f7549757f5a0daf2807df1c346231659f5522659f3",
        )),
        ("macos", "aarch64") => Some((
            "kitten-darwin-arm64",
            "97963e3885012e3d8fa711cfc3201db3380002eb9927f230f3b08400f1aec7f0",
        )),
        _ => None,
    }
}

pub fn check() -> Outcome {
    if asset().is_none() {
        return Ok(Status::Unsupported);
    }
    let Some(path) = executable() else {
        return Ok(Status::Missing);
    };
    let mut command = std::process::Command::new(path);
    command.arg("--version");
    let bytes = super::capture_with_timeout(&mut command, std::time::Duration::from_secs(2))
        .ok_or("kitten --version failed or timed out")?;
    let version = String::from_utf8_lossy(&bytes).trim().to_string();
    if version.len() > 128 || !version.to_lowercase().starts_with("kitten ") {
        return Err("unexpected kitten --version response".into());
    }
    Ok(Status::Installed(version))
}

pub fn perform(install: bool) -> Outcome {
    if !install {
        return check();
    }
    let Some((name, checksum)) = asset() else {
        return Ok(Status::Unsupported);
    };
    if matches!(check(), Ok(Status::Installed(_))) {
        return check();
    }
    #[cfg(not(windows))]
    {
        let destination = managed_path();
        let directory = destination.parent().ok_or("invalid helper directory")?;
        crate::persist::ensure_private_server_dir(directory).map_err(|e| e.to_string())?;
        let staged = directory.join(crate::ids::public_id("kitten-download"));
        let result = (|| {
            let url =
                format!("https://github.com/kovidgoyal/kitty/releases/download/v0.48.2/{name}");
            crate::update::download_file_bounded(&url, &staged, Some(40 * 1024 * 1024))
                .map_err(|e| e.to_string())?;
            verify_and_replace(&staged, &destination, checksum)?;
            check()
        })();
        let _ = std::fs::remove_file(staged);
        result
    }
    #[cfg(windows)]
    {
        let _ = (name, checksum);
        Ok(Status::Unsupported)
    }
}

#[cfg(not(windows))]
fn verify_and_replace(
    staged: &std::path::Path,
    destination: &std::path::Path,
    checksum: &str,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(staged).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buf = [0; 16 * 1024];
    loop {
        let count = file.read(&mut buf).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buf[..count]);
    }
    if format!("{:x}", digest.finalize()) != checksum {
        return Err("kitten SHA-256 mismatch; existing installation was not changed".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    crate::platform::atomic_replace_file(staged, destination).map_err(|e| e.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn helper_checksum_failure_preserves_existing_installation() {
        let _env = crate::persist::test_env("kitten-install");
        let destination = managed_path();
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"original").unwrap();
        let staged = destination.with_extension("download");
        let data = b"#!/bin/sh\nprintf 'kitten 0.48.2\\n'\n";
        std::fs::write(&staged, data).unwrap();
        assert!(verify_and_replace(&staged, &destination, &"0".repeat(64)).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
        verify_and_replace(
            &staged,
            &destination,
            &format!("{:x}", Sha256::digest(data)),
        )
        .unwrap();
        assert_eq!(executable(), Some(destination));
        assert_eq!(check().unwrap(), Status::Installed("kitten 0.48.2".into()));
        assert_eq!(
            perform(true).unwrap(),
            Status::Installed("kitten 0.48.2".into()),
            "repeated install must not download"
        );
    }
}

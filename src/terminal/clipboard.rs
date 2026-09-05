//! Display-client clipboard acquisition and owner-server image staging.
//! No clipboard is read on the remote host; image bytes use the same private
//! client transport as text input, then a path local to the owning PTY is pasted.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardImage {
    pub extension: String,
    pub bytes: Vec<u8>,
}

impl ClipboardImage {
    pub fn valid(&self) -> bool {
        if self.bytes.is_empty() || self.bytes.len() > MAX_IMAGE_BYTES {
            return false;
        }
        let bytes = &self.bytes;
        match self.extension.as_str() {
            "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "jpg" => bytes.starts_with(b"\xff\xd8\xff"),
            "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            "webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            "bmp" => bytes.starts_with(b"BM"),
            _ => false,
        }
    }
}

/// External clipboard helpers are optional and bounded. A missing image leaves
/// Ctrl+V untouched, so text-paste shortcuts and child bindings keep working.
fn capture(mut command: Command) -> Option<Vec<u8>> {
    capture_with_timeout(&mut command, Duration::from_secs(1))
}

fn capture_with_timeout(command: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::no_window(command);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take((MAX_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= MAX_IMAGE_BYTES).then_some(bytes)
    });
    let deadline = Instant::now() + timeout;
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let bytes = reader.join().ok().flatten()?;
    success.then_some(bytes)
}

#[cfg(any(target_os = "linux", windows))]
fn image_from(command: Command, extension: &str) -> Option<ClipboardImage> {
    let image = ClipboardImage {
        extension: extension.to_string(),
        bytes: capture(command)?,
    };
    image.valid().then_some(image)
}

pub fn read_image() -> Option<ClipboardImage> {
    #[cfg(any(target_os = "linux", windows))]
    {
        if cfg!(windows) || std::env::var_os("WSL_DISTRO_NAME").is_some() {
            let mut command = Command::new(if cfg!(windows) {
                "powershell"
            } else {
                "powershell.exe"
            });
            command.args(["-NoProfile", "-NonInteractive", "-STA", "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; $clip=[Windows.Forms.Clipboard]::GetImage(); if ($null -eq $clip) { exit 1 }; $buffer=New-Object IO.MemoryStream; try { $clip.Save($buffer,[Drawing.Imaging.ImageFormat]::Png); $data=$buffer.ToArray(); [Console]::OpenStandardOutput().Write($data,0,$data.Length) } finally { $clip.Dispose(); $buffer.Dispose() }"]);
            if let Some(image) = image_from(command, "png") {
                return Some(image);
            }
        }
    }
    #[cfg(target_os = "linux")]
    for (mime, extension) in [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/webp", "webp"),
        ("image/gif", "gif"),
        ("image/bmp", "bmp"),
    ] {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let mut command = Command::new("wl-paste");
            command.args(["--no-newline", "--type", mime]);
            if let Some(image) = image_from(command, extension) {
                return Some(image);
            }
        }
        if std::env::var_os("DISPLAY").is_some() {
            let mut command = Command::new("xclip");
            command.args(["-selection", "clipboard", "-target", mime, "-out"]);
            if let Some(image) = image_from(command, extension) {
                return Some(image);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::temp_dir().join(crate::ids::public_id("luvus-clipboard"));
        std::fs::create_dir(&dir).ok()?;
        crate::persist::ensure_private_server_dir(&dir).ok()?;
        let path = dir.join("image.png");
        let mut command = Command::new("osascript");
        command.args(["-e", "on run argv\nset imageData to the clipboard as «class PNGf»\nset outputFile to open for access POSIX file (item 1 of argv) with write permission\ntry\nwrite imageData to outputFile\nclose access outputFile\non error\nclose access outputFile\nerror\nend try\nend run", "--"]).arg(&path);
        let result = capture(command).and_then(|_| {
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .ok()?
                .take((MAX_IMAGE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .ok()?;
            let image = ClipboardImage {
                extension: "png".into(),
                bytes,
            };
            image.valid().then_some(image)
        });
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
        return result.or_else(read_kitty_image);
    }
    #[allow(unreachable_code)]
    read_kitty_image()
}

/// The input reader calls this synchronously between Crossterm reads, so the
/// official helper is the only reader of the controlling TTY during its OSC
/// 5522 transaction. It implements MIME negotiation, chunking and permission
/// denial; never query clipboard data on startup or relax Kitty permissions.
fn read_kitty_image() -> Option<ClipboardImage> {
    if !std::env::var("TERM").is_ok_and(|term| term.contains("kitty"))
        && std::env::var_os("KITTY_WINDOW_ID").is_none()
    {
        return None;
    }
    let mut command = Command::new("kitten");
    command.args([
        "clipboard",
        "--get-clipboard",
        "--mime",
        "image/png",
        "/dev/stdout",
    ]);
    let image = ClipboardImage {
        extension: "png".into(),
        // The terminal may ask the user to authorize this explicit paste.
        bytes: capture_with_timeout(&mut command, Duration::from_secs(30))?,
    };
    image.valid().then_some(image)
}

/// Stage only validated image formats, using a random owner-private filename.
/// Keep the file across client detach: agents may read the attachment later.
pub fn stage(image: &ClipboardImage) -> Result<PathBuf, String> {
    if !image.valid() {
        return Err("invalid or oversized clipboard image".into());
    }
    let dir = crate::session::active_dir().join("clipboard");
    crate::persist::ensure_private_server_dir(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!(
        "{}.{}",
        crate::ids::public_id("image"),
        image.extension
    ));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&path)?;
        file.write_all(&image.bytes)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&path);
        return Err(error.to_string());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn ssh_kitty_reads_desktop_image_without_a_linux_display() {
        use std::os::unix::fs::PermissionsExt;
        let _env = crate::persist::test_env("kitty-clipboard");
        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
        let _restore = Restore(
            [
                "PATH",
                "TERM",
                "DISPLAY",
                "WAYLAND_DISPLAY",
                "WSL_DISTRO_NAME",
            ]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
        );
        let dir = crate::persist::config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let helper = dir.join("kitten");
        std::fs::write(&helper, "#!/bin/sh\n[ \"$1\" = clipboard ] && [ \"$2\" = --get-clipboard ] || exit 1\nprintf '\\211PNG\\r\\n\\032\\nfixture'\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("PATH", &dir);
        std::env::set_var("TERM", "xterm-kitty");
        for key in ["DISPLAY", "WAYLAND_DISPLAY", "WSL_DISTRO_NAME"] {
            std::env::remove_var(key);
        }
        let image = read_image().expect("read the desktop clipboard through Kitty, not X11");
        assert!(image.valid());
        assert_eq!(image.extension, "png");
    }

    #[test]
    fn validates_clipboard_image_payload_and_stages_private_bytes() {
        let _env = crate::persist::test_env("clipboard-images");
        let image = ClipboardImage {
            extension: "png".into(),
            bytes: b"\x89PNG\r\n\x1a\ntest".to_vec(),
        };
        let path = stage(&image).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), image.bytes);
        assert_ne!(stage(&image).unwrap(), path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        }
        assert!(!ClipboardImage {
            extension: "../sh".into(),
            ..image.clone()
        }
        .valid());
        assert!(!ClipboardImage {
            bytes: b"text pretending to be an image".to_vec(),
            ..image.clone()
        }
        .valid());
        assert!(!ClipboardImage {
            bytes: vec![0; MAX_IMAGE_BYTES + 1],
            ..image
        }
        .valid());
    }
}

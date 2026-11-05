//! Owner-side file access rooted in a trusted, core-selected workspace.
//! Construct and use this helper off the App loop, never from a display path.

use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub(crate) struct WorkspaceFiles {
    #[cfg(unix)]
    root: PathBuf,
    #[cfg(unix)]
    original_root: PathBuf,
    #[cfg(unix)]
    directory: std::os::fd::OwnedFd,
}

/// Reserve before requesting a capture. The slot outlives its pending RPC
/// while disk I/O finishes; two slow writes cannot grow an unbounded queue.
pub(super) struct Screenshot {
    root: PathBuf,
    output: PathBuf,
    slots: Arc<AtomicUsize>,
    authority: Arc<AtomicBool>,
    deadline: Instant,
}

impl Screenshot {
    pub(super) fn reserve(
        root: &Path,
        output: &str,
        slots: &Arc<AtomicUsize>,
        authority: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<Self, &'static str> {
        if !cfg!(unix) {
            return Err("unsupported");
        }
        if slots.fetch_add(1, Ordering::AcqRel) >= 2 {
            slots.fetch_sub(1, Ordering::AcqRel);
            return Err("limit_exceeded");
        }
        Ok(Self {
            root: root.into(),
            output: output.into(),
            slots: slots.clone(),
            authority: authority.clone(),
            deadline,
        })
    }

    pub(super) fn save(&self, data: &Value) -> Result<Value, &'static str> {
        let cancelled =
            || !self.authority.load(Ordering::Acquire) || Instant::now() >= self.deadline;
        if cancelled() {
            return Err("control_revoked");
        }
        let encoded = data["base64"].as_str().ok_or("display_failed")?;
        let bytes =
            crate::terminal::clipboard::decode_base64(encoded, super::control::MAX_RESULT_BYTES)
                .map_err(|_| "display_failed")?;
        if data["mime_type"] != "image/png" || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("display_failed");
        }
        let files = WorkspaceFiles::open(&self.root).map_err(|_| "file_denied")?;
        files
            .write_new(&self.output, &bytes, cancelled)
            .map_err(|e| match e.kind() {
                io::ErrorKind::AlreadyExists => "file_exists",
                io::ErrorKind::Interrupted => "control_revoked",
                _ => "file_denied",
            })?;
        Ok(
            json!({"path":self.root.join(&self.output), "bytes":bytes.len(), "mime_type":"image/png",
            "document_revision":data["document_revision"]}),
        )
    }
}

impl Drop for Screenshot {
    fn drop(&mut self) {
        self.slots.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(not(unix))]
impl WorkspaceFiles {
    pub(crate) fn open(_root: &Path) -> io::Result<Self> {
        Err(unsupported())
    }

    pub(crate) fn write_new(
        &self,
        _path: &Path,
        _bytes: &[u8],
        _cancelled: impl Fn() -> bool,
    ) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "workspace file transfer is unsupported on this platform",
    )
}

#[cfg(unix)]
mod unix {
    use super::{io, Path, WorkspaceFiles};
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    const MAX_BYTES: usize = 64 * 1024 * 1024;
    const CHUNK_BYTES: usize = 64 * 1024;
    const DIRECTORY_FLAGS: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    impl WorkspaceFiles {
        /// `root` must come from stored workspace metadata, not page parameters.
        pub(crate) fn open(root: &Path) -> io::Result<Self> {
            validate_path(root)?;
            if !root.is_absolute() {
                return Err(invalid_path());
            }
            // Only the trusted root may resolve existing symlink aliases. Walk
            // its canonical spelling without following any later symlink swap.
            let canonical = std::fs::canonicalize(root)
                .map_err(|error| io::Error::new(error.kind(), "cannot resolve workspace root"))?;
            // This absolute constant does not depend on the ambient cwd.
            let mut directory = open_at(libc::AT_FDCWD, c"/", DIRECTORY_FLAGS, 0)?;
            for component in canonical.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => {
                        let name = c_name(name.as_bytes())?;
                        directory = open_at(directory.as_raw_fd(), &name, DIRECTORY_FLAGS, 0)?;
                    }
                    _ => return Err(invalid_path()),
                }
            }
            Ok(Self {
                root: canonical,
                original_root: root.to_path_buf(),
                directory,
            })
        }

        pub(crate) fn subdirectory(&self, path: &Path) -> io::Result<Self> {
            validate_path(path)?;
            // parent walks directories with O_NOFOLLOW and pins the final
            // directory. No placeholder file is opened or created.
            let (directory, _) = self.parent(&path.join(".luvus-directory-probe"), &|| false)?;
            let original_root = self.original_root.join(path);
            Ok(Self {
                root: original_root.clone(),
                original_root,
                directory,
            })
        }

        pub(crate) fn read(
            &self,
            path: &Path,
            limit: usize,
            cancelled: impl Fn() -> bool,
        ) -> io::Result<Vec<u8>> {
            if limit > MAX_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "file read limit exceeds 64 MiB",
                ));
            }
            let (parent, name) = self.parent(path, &cancelled)?;
            check_cancelled(&cancelled)?;
            // NONBLOCK avoids waiting on a FIFO before fstat; NOCTTY avoids
            // acquiring a controlling terminal before rejecting a device.
            let descriptor = open_at(
                parent.as_raw_fd(),
                &name,
                libc::O_RDONLY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK
                    | libc::O_NOCTTY,
                0,
            )?;
            let mut file = File::from(descriptor);
            // File::metadata inspects the open descriptor, not the pathname.
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace transfer requires a regular file",
                ));
            }
            if metadata.len() > limit as u64 {
                return Err(too_large());
            }
            // SAFETY: file owns a live descriptor; these commands take no pointer.
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: F_SETFL changes only flags on this owned descriptor.
            let result =
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }

            check_cancelled(&cancelled)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(metadata.len() as usize)
                .map_err(|_| io::Error::other("cannot allocate file transfer buffer"))?;
            let mut chunk = [0_u8; CHUNK_BYTES];
            loop {
                check_cancelled(&cancelled)?;
                // Read at most cap + 1 in total, even if the file grows. The
                // extra byte detects overflow without storing it in the result.
                let available = chunk.len().min(limit + 1 - bytes.len());
                let count = match file.read(&mut chunk[..available]) {
                    Ok(count) => count,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                };
                check_cancelled(&cancelled)?;
                if count == 0 {
                    return Ok(bytes);
                }
                if count > limit - bytes.len() {
                    return Err(too_large());
                }
                // Exact reservation avoids Vec's geometric growth overshooting
                // the cap when a file grows after the metadata check.
                bytes
                    .try_reserve_exact(count)
                    .map_err(|_| io::Error::other("cannot allocate file transfer buffer"))?;
                bytes.extend_from_slice(&chunk[..count]);
            }
        }

        pub(crate) fn write_new(
            &self,
            path: &Path,
            bytes: &[u8],
            cancelled: impl Fn() -> bool,
        ) -> io::Result<()> {
            if bytes.len() > MAX_BYTES {
                return Err(too_large());
            }
            self.publish(path, &cancelled, |file| {
                for chunk in bytes.chunks(CHUNK_BYTES) {
                    check_cancelled(&cancelled)?;
                    file.write_all(chunk)?;
                }
                Ok(())
            })
        }

        pub(crate) fn write_download(
            &self,
            name: &Path,
            size: usize,
            stream: std::sync::mpsc::Receiver<Option<Vec<u8>>>,
            cancelled: impl Fn() -> bool,
        ) -> io::Result<()> {
            if size > MAX_BYTES {
                return Err(too_large());
            }
            self.publish(name, &cancelled, |file| {
                let mut remaining = size;
                while let Ok(chunk) = stream.recv() {
                    check_cancelled(&cancelled)?;
                    match chunk {
                        None if remaining == 0 => return Ok(()),
                        Some(bytes) if !bytes.is_empty() && bytes.len() <= remaining => {
                            file.write_all(&bytes)?;
                            remaining -= bytes.len();
                        }
                        _ => return Err(too_large()),
                    }
                }
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "incomplete download",
                ))
            })
        }

        pub(crate) fn destination(&self, name: &str) -> std::path::PathBuf {
            self.root.join(name)
        }

        fn publish(
            &self,
            path: &Path,
            cancelled: &impl Fn() -> bool,
            write: impl FnOnce(&mut File) -> io::Result<()>,
        ) -> io::Result<()> {
            let (parent, name) = self.parent(path, &cancelled)?;
            let mut staging = Staging::create(&parent, &cancelled)?;
            check_cancelled(&cancelled)?;
            let mut file = File::from(open_at(
                staging.directory(),
                c"payload",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )?);
            // Never remove an existing payload if exclusive creation failed.
            staging.has_payload = true;
            write(&mut file)?;
            check_cancelled(&cancelled)?;
            file.sync_all()?;
            check_cancelled(&cancelled)?;
            // SAFETY: both dirfds remain pinned and both names are NUL-terminated
            // single components. The private directory protects the source name.
            let result = unsafe {
                libc::linkat(
                    staging.directory(),
                    c"payload".as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    0,
                )
            };
            if result < 0 {
                // In particular, EEXIST also rejects a final symlink.
                return Err(io::Error::last_os_error());
            }
            // Publication is the commit point. Never return cancellation or a
            // cleanup error afterward, and never unlink the final destination.
            Ok(())
        }

        pub(crate) fn parent(
            &self,
            path: &Path,
            cancelled: &impl Fn() -> bool,
        ) -> io::Result<(OwnedFd, CString)> {
            check_cancelled(cancelled)?;
            validate_path(path)?;
            // components()/file_name() normalize trailing slash and dot; check
            // the raw last component first so neither becomes a destination.
            match path
                .as_os_str()
                .as_bytes()
                .rsplit(|byte| *byte == b'/')
                .next()
            {
                Some(b"" | b"." | b"..") | None => return Err(invalid_path()),
                Some(_) => {}
            }
            let relative = if path.is_absolute() {
                // Prefer the original alias when it is itself inside the
                // canonical root, rather than trying to traverse that symlink.
                path.strip_prefix(&self.original_root)
                    .or_else(|_| path.strip_prefix(&self.root))
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "file path is outside the workspace",
                        )
                    })?
            } else {
                path
            };
            let mut components = relative.components();
            let Some(Component::Normal(name)) = components.next_back() else {
                return Err(invalid_path());
            };
            let name = c_name(name.as_bytes())?;
            let mut parent = self.directory.try_clone()?;
            for component in components {
                check_cancelled(cancelled)?;
                match component {
                    Component::CurDir => {}
                    Component::Normal(name) => {
                        let name = c_name(name.as_bytes())?;
                        parent = open_at(parent.as_raw_fd(), &name, DIRECTORY_FLAGS, 0)?;
                    }
                    _ => return Err(invalid_path()),
                }
            }
            Ok((parent, name))
        }
    }

    fn validate_path(path: &Path) -> io::Result<()> {
        let bytes = path.as_os_str().as_bytes();
        if bytes.is_empty()
            || bytes.contains(&0)
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(invalid_path());
        }
        Ok(())
    }

    fn invalid_path() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, "invalid workspace file path")
    }

    fn too_large() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds transfer byte limit",
        )
    }

    fn check_cancelled(cancelled: &impl Fn() -> bool) -> io::Result<()> {
        if cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "file transfer cancelled",
            ))
        } else {
            Ok(())
        }
    }

    fn c_name(bytes: &[u8]) -> io::Result<CString> {
        CString::new(bytes).map_err(|_| invalid_path())
    }

    fn open_at(
        parent: RawFd,
        name: &CStr,
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> io::Result<OwnedFd> {
        // SAFETY: callers keep the parent alive (or use an absolute constant).
        // name is NUL-terminated; promote mode_t for C varargs (u16 on macOS).
        let descriptor =
            unsafe { libc::openat(parent, name.as_ptr(), flags, mode as libc::c_uint) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new descriptor, owned exactly once here.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }

    fn staging_identity(parent: RawFd, name: &CStr) -> io::Result<(libc::dev_t, libc::ino_t)> {
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is pinned, name is NUL-terminated, and metadata is
        // writable storage for one stat. Do not follow a substituted symlink.
        if unsafe {
            libc::fstatat(
                parent,
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatat initialized metadata; geteuid takes no args.
        let metadata = unsafe { metadata.assume_init() };
        let owner = unsafe { libc::geteuid() };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
            || metadata.st_mode & 0o077 != 0
            || metadata.st_uid != owner
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "staging directory is not private",
            ));
        }
        Ok((metadata.st_dev, metadata.st_ino))
    }

    struct Staging<'a> {
        parent: &'a OwnedFd,
        name: CString,
        directory: Option<OwnedFd>,
        identity: Option<(libc::dev_t, libc::ino_t)>,
        has_payload: bool,
    }

    impl<'a> Staging<'a> {
        fn create(parent: &'a OwnedFd, cancelled: &impl Fn() -> bool) -> io::Result<Self> {
            for _ in 0..16 {
                check_cancelled(cancelled)?;
                let mut random = [0_u8; 16];
                getrandom::fill(&mut random)
                    .map_err(|_| io::Error::other("cannot obtain staging randomness"))?;
                let name = c_name(
                    format!(".luvus-transfer-{:032x}", u128::from_ne_bytes(random)).as_bytes(),
                )?;
                // SAFETY: parent is pinned, name is a single NUL-terminated
                // component, and mkdirat creates exclusively with private mode.
                if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        continue;
                    }
                    return Err(error);
                }
                // Arm cleanup before opening the directory or its payload.
                // Record identity so a renamed/replaced directory is not removed.
                let mut staging = Self {
                    parent,
                    name,
                    directory: None,
                    identity: None,
                    has_payload: false,
                };
                staging.identity = Some(staging_identity(parent.as_raw_fd(), &staging.name)?);
                staging.directory = Some(open_at(
                    parent.as_raw_fd(),
                    &staging.name,
                    DIRECTORY_FLAGS,
                    0,
                )?);
                if Some(staging_identity(staging.directory(), c".")?) != staging.identity {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "staging directory changed during creation",
                    ));
                }
                return Ok(staging);
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "cannot create private file staging directory",
            ))
        }

        fn directory(&self) -> RawFd {
            self.directory
                .as_ref()
                .expect("successful staging creation pins its directory")
                .as_raw_fd()
        }
    }

    impl Drop for Staging<'_> {
        fn drop(&mut self) {
            if self.has_payload {
                if let Some(directory) = &self.directory {
                    // SAFETY: the pinned private directory owns this created
                    // name. Never traverse it or recursively remove anything.
                    unsafe { libc::unlinkat(directory.as_raw_fd(), c"payload".as_ptr(), 0) };
                }
            }
            if let Some(identity) = self.identity {
                if staging_identity(self.parent.as_raw_fd(), &self.name).ok() == Some(identity) {
                    // SAFETY: parent is still pinned; this name still identifies
                    // our empty directory. Never recursively remove contents or
                    // try another name if cleanup cannot establish its identity.
                    unsafe {
                        libc::unlinkat(
                            self.parent.as_raw_fd(),
                            self.name.as_ptr(),
                            libc::AT_REMOVEDIR,
                        )
                    };
                }
            }
        }
    }
}

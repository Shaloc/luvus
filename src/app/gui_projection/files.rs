use super::App;
use crate::ipc::app_surface::protocol::FileOperation;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

impl App {
    pub(crate) fn desktop_file_entry(
        &mut self,
        workspace: &str,
        root: &Path,
        path: PathBuf,
        operation: FileOperation,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) -> Result<bool, &'static str> {
        if !self
            .workspaces
            .get(self.active_ws)
            .is_some_and(|ws| ws.id == workspace && ws.remote.is_none() && ws.cwd == root)
            || self.file_tree.root() != root
            || self.preparing_registered_workspace()
        {
            return Err("workspace changed");
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| "path outside workspace")?;
        let create = matches!(
            operation,
            FileOperation::CreateFile { .. } | FileOperation::CreateDirectory { .. }
        );
        if (rel.as_os_str().is_empty() && !create)
            || rel
                .components()
                .any(|c| !matches!(c,Component::Normal(s) if !s.eq_ignore_ascii_case(".git")))
        {
            return Err("invalid workspace path");
        }
        let (is_dir, expanded) = if path == root {
            (true, true)
        } else {
            let row = self
                .file_tree
                .tree_rows()
                .iter()
                .find(|row| row.path == path)
                .ok_or("file entry changed")?;
            (row.is_dir, row.expanded)
        };
        if create && !is_dir {
            return Err("choose a folder");
        }
        match operation {
            FileOperation::Markdown | FileOperation::Mermaid if !is_dir => {
                let kind = if matches!(operation, FileOperation::Markdown) {
                    crate::files::preview::PreviewKind::Markdown
                } else {
                    crate::files::preview::PreviewKind::Mermaid
                };
                self.open_document_preview_tab(path, kind);
                done(Ok(json!({"opened":true})));
                return Ok(true);
            }
            FileOperation::Expand | FileOperation::Collapse if is_dir => {
                let expand = matches!(operation, FileOperation::Expand);
                if expanded == expand {
                    done(Ok(json!({"expanded":expand})));
                    return Ok(false);
                }
                if !expand {
                    self.file_tree.toggle(&path);
                    done(Ok(json!({"expanded":false})));
                    return Ok(true);
                }
                let root = root.to_owned();
                let ws = workspace.to_owned();
                let read = path.clone();
                self.io_jobs.submit(self.app_tx.clone(), move || {
                    let entries = crate::files::read_dir_entries(&read);
                    Box::new(move |app| {
                        if app.file_tree.root() != root
                            || !app
                                .workspaces
                                .get(app.active_ws)
                                .is_some_and(|w| w.id == ws)
                        {
                            done(Err("workspace changed".into()));
                            return false;
                        }
                        app.file_tree.apply_dir(read, entries);
                        done(Ok(json!({"expanded":true})));
                        true
                    })
                })?;
                self.file_tree.toggle(&path);
                return Ok(true);
            }
            FileOperation::Markdown
            | FileOperation::Mermaid
            | FileOperation::Expand
            | FileOperation::Collapse => return Err("wrong file entry kind"),
            _ => {}
        }
        let mutation = !matches!(operation, FileOperation::Inspect);
        if mutation && self.file_mutation_inflight {
            return Err("another file operation is pending");
        }
        let root = root.to_owned();
        let ws = workspace.to_owned();
        self.io_jobs.submit(self.app_tx.clone(), move || {
            let result = edit_entry(&root, &path, &operation).map_err(|e| e.to_string());
            Box::new(move |app| {
                if mutation {
                    app.file_mutation_inflight = false;
                    if result.is_ok()
                        && app.file_tree.root() == root
                        && app
                            .workspaces
                            .get(app.active_ws)
                            .is_some_and(|w| w.id == ws)
                    {
                        let changed = match operation {
                            FileOperation::Rename { name, .. } => path.with_file_name(name),
                            FileOperation::CreateFile { name }
                            | FileOperation::CreateDirectory { name } => path.join(name),
                            _ => path,
                        };
                        app.after_fs_change(&changed);
                    }
                }
                done(result);
                mutation
            })
        })?;
        if mutation {
            self.file_mutation_inflight = true;
        }
        Ok(false)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn edit_entry(root: &Path, path: &Path, op: &FileOperation) -> std::io::Result<Value> {
    use std::ffi::CString;
    use std::io::{Error, ErrorKind};
    use std::os::fd::AsRawFd;
    let workspace = crate::browser::WorkspaceFiles::open(root)?;
    if let FileOperation::CreateFile { name } | FileOperation::CreateDirectory { name } = op {
        use std::os::fd::{FromRawFd, OwnedFd};
        let (parent, new) = workspace.parent(&path.join(name), &|| false)?;
        if matches!(op, FileOperation::CreateFile { .. }) {
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    new.as_ptr(),
                    libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_WRONLY
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW,
                    0o600 as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(Error::last_os_error());
            }
            drop(unsafe { OwnedFd::from_raw_fd(fd) });
        } else if unsafe { libc::mkdirat(parent.as_raw_fd(), new.as_ptr(), 0o700) } < 0 {
            return Err(Error::last_os_error());
        }
        return Ok(json!({"created":true}));
    }
    let (parent, source) = workspace.parent(path, &|| false)?;
    let fd = parent.as_raw_fd();
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // Inspect the selected directory entry itself; neither parent links nor
    // a final symlink can redirect mutations into a different directory.
    if unsafe {
        libc::fstatat(
            fd,
            source.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(Error::last_os_error());
    }
    let meta = unsafe { metadata.assume_init() };
    let identity = format!(
        "{:x}:{:x}:{:x}:{:x}:{:x}",
        meta.st_dev, meta.st_ino, meta.st_mode, meta.st_ctime, meta.st_ctime_nsec
    );
    let expected = match op {
        FileOperation::Inspect => {
            return Ok(
                json!({"identity":identity,"directory":meta.st_mode&libc::S_IFMT==libc::S_IFDIR}),
            )
        }
        FileOperation::Rename { identity, .. } | FileOperation::Delete { identity } => identity,
        _ => return Err(Error::new(ErrorKind::InvalidInput, "not an entry mutation")),
    };
    if expected != &identity {
        return Err(Error::other("entry changed since confirmation"));
    }
    let result = match op {
        FileOperation::Rename { name, .. } => {
            let dest = CString::new(name.as_bytes())?;
            // Atomic no-overwrite, including dangling links and an entry
            // created after the user confirmed the new name.
            #[cfg(target_os = "macos")]
            unsafe {
                libc::renameatx_np(fd, source.as_ptr(), fd, dest.as_ptr(), libc::RENAME_EXCL)
            }
            #[cfg(target_os = "linux")]
            unsafe {
                libc::renameat2(
                    fd,
                    source.as_ptr(),
                    fd,
                    dest.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            }
        }
        FileOperation::Delete { .. } => {
            let flags = if meta.st_mode & libc::S_IFMT == libc::S_IFDIR {
                libc::AT_REMOVEDIR
            } else {
                0
            };
            unsafe { libc::unlinkat(fd, source.as_ptr(), flags) }
        }
        _ => unreachable!(),
    };
    if result < 0 {
        return Err(Error::last_os_error());
    }
    Ok(json!({"changed":true}))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn edit_entry(_: &Path, _: &Path, _: &FileOperation) -> std::io::Result<Value> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure file mutations unavailable on this owner",
    ))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    #[test]
    fn desktop_file_edits_reject_changed_identity_overwrite_and_parent_links() {
        let _env = crate::persist::test_env("desktop-file-safety");
        let home = crate::persist::config_dir();
        let root = home.join("root");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("before");
        fs::write(&source, "first").unwrap();
        let identity = edit_entry(&root, &source, &FileOperation::Inspect).unwrap()["identity"]
            .as_str()
            .unwrap()
            .to_string();
        fs::write(root.join("keep"), "keep").unwrap();
        assert!(edit_entry(
            &root,
            &source,
            &FileOperation::Rename {
                name: "keep".into(),
                identity: identity.clone()
            }
        )
        .is_err());
        fs::rename(&source, root.join("first")).unwrap();
        fs::write(&source, "replacement").unwrap();
        assert!(edit_entry(&root, &source, &FileOperation::Delete { identity }).is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
        symlink(&home, root.join("alias")).unwrap();
        assert!(edit_entry(
            &root,
            &root.join("alias/root/before"),
            &FileOperation::Inspect
        )
        .is_err());
        assert_eq!(fs::read_to_string(root.join("keep")).unwrap(), "keep");
    }
}

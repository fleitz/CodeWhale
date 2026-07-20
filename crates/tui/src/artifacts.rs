//! Session-scoped artifact metadata.
//!
//! Large tool outputs are written under the owning session directory and saved
//! sessions keep a durable metadata index for resume/listing flows.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const ARTIFACTS_DIR_NAME: &str = "artifacts";

#[cfg(test)]
static TEST_ARTIFACT_SESSIONS_ROOT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) static TEST_ARTIFACT_SESSIONS_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(test, unix))]
static BEFORE_SESSION_ARTIFACT_LEAF_OPEN_HOOK: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);

#[cfg(all(test, unix))]
static BEFORE_SESSION_ARTIFACT_RENAME_HOOK: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    ToolOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub kind: ArtifactKind,
    #[serde(default)]
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    /// Terminal result status when known. `None` is the backward-compatible
    /// shape for artifacts saved before status persistence existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub byte_size: u64,
    pub preview: String,
    pub storage_path: PathBuf,
}

fn sanitize_id_component(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[must_use]
pub fn artifact_id_for_tool_call(tool_call_id: &str) -> String {
    format!("art_{}", sanitize_id_component(tool_call_id))
}

#[must_use]
pub fn session_artifact_relative_path(artifact_id: &str) -> PathBuf {
    PathBuf::from(ARTIFACTS_DIR_NAME).join(format!("{artifact_id}.txt"))
}

fn session_artifact_relative_path_with_extension(
    artifact_id: &str,
    extension: &str,
) -> io::Result<PathBuf> {
    let artifact_id = sanitize_id_component(artifact_id);
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    if artifact_id.is_empty()
        || extension.is_empty()
        || !extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact id and extension must contain safe ASCII characters",
        ));
    }
    Ok(PathBuf::from(ARTIFACTS_DIR_NAME).join(format!("{artifact_id}.{extension}")))
}

fn artifact_sessions_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_ARTIFACT_SESSIONS_ROOT
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
    {
        return Some(root);
    }

    let home = dirs::home_dir()?;
    let primary = home.join(".codewhale").join("sessions");
    let legacy = home.join(".deepseek").join("sessions");
    if primary.exists() || !legacy.exists() {
        return Some(primary);
    }
    Some(legacy)
}

#[cfg(test)]
pub(crate) fn set_test_artifact_sessions_root(root: Option<PathBuf>) -> Option<PathBuf> {
    let mut guard = TEST_ARTIFACT_SESSIONS_ROOT
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    std::mem::replace(&mut *guard, root)
}

#[must_use]
pub fn session_artifact_absolute_path(session_id: &str, relative_path: &Path) -> Option<PathBuf> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(
        artifact_sessions_root()?
            .join(session_id)
            .join(relative_path),
    )
}

fn validate_artifact_relative_path(relative_path: &Path) -> io::Result<()> {
    if relative_path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact path must be relative",
        ));
    }
    let mut components = relative_path.components();
    if components.next() != Some(Component::Normal(ARTIFACTS_DIR_NAME.as_ref()))
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact path must stay below the session artifacts directory",
        ));
    }
    Ok(())
}

fn checked_directory(path: &Path, trusted_root: &Path) -> io::Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact directory symlink refused",
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact path parent is not a directory",
        ));
    }
    let resolved = path.canonicalize()?;
    if !resolved.starts_with(trusted_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact directory escaped the sessions root",
        ));
    }
    Ok(resolved)
}

fn secure_session_artifact_path(
    session_id: &str,
    relative_path: &Path,
    create_parents: bool,
) -> io::Result<PathBuf> {
    if !is_valid_session_id(session_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid artifact session",
        ));
    }
    validate_artifact_relative_path(relative_path)?;
    let sessions_root = artifact_sessions_root().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve artifact sessions root",
        )
    })?;
    if create_parents {
        std::fs::create_dir_all(&sessions_root)?;
    }
    let root_metadata = std::fs::symlink_metadata(&sessions_root)?;
    if root_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact sessions root symlink refused",
        ));
    }
    if !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact sessions root is not a directory",
        ));
    }
    let trusted_root = sessions_root.canonicalize()?;
    let mut current = sessions_root.join(session_id);
    let mut parents = vec![current.clone()];
    let relative_parent = relative_path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "artifact path has no parent")
    })?;
    for component in relative_parent.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid artifact parent component",
            ));
        };
        current.push(component);
        parents.push(current.clone());
    }
    for parent in parents {
        if create_parents {
            match std::fs::create_dir(&parent) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        checked_directory(&parent, &trusted_root)?;
    }

    let candidate = sessions_root.join(session_id).join(relative_path);
    if let Ok(metadata) = std::fs::symlink_metadata(&candidate) {
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact symlink refused",
            ));
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact is not a regular file",
            ));
        }
    }
    Ok(candidate)
}

/// Resolve the owning session's artifacts directory after applying the same
/// ancestor no-symlink checks used for individual reads. Callers that must
/// enumerate compatibility artifacts use this instead of canonicalizing a
/// caller-composed path.
pub(crate) fn resolve_session_artifacts_dir_for_read(session_id: &str) -> io::Result<PathBuf> {
    let probe = secure_session_artifact_path(
        session_id,
        &PathBuf::from(ARTIFACTS_DIR_NAME).join(".codewhale-read-probe"),
        false,
    )?;
    probe
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid artifacts directory"))?
        .canonicalize()
}

/// Resolve an existing session artifact without following symlinks in any
/// session-owned path component or permitting cross-session traversal.
pub fn resolve_session_artifact_for_read(
    session_id: &str,
    relative_path: &Path,
) -> io::Result<PathBuf> {
    let candidate = secure_session_artifact_path(session_id, relative_path, false)?;
    let metadata = std::fs::symlink_metadata(&candidate)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact symlink refused",
        ));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact is not a regular file",
        ));
    }
    let artifact_root = candidate
        .parent()
        .and_then(|_| artifact_sessions_root())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid artifact root"))?
        .join(session_id)
        .join(ARTIFACTS_DIR_NAME)
        .canonicalize()?;
    let resolved = candidate.canonicalize()?;
    if !resolved.starts_with(&artifact_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact escaped its session root",
        ));
    }
    Ok(resolved)
}

/// Open one existing session artifact through descriptor-relative traversal.
///
/// On Unix every path component is opened from its already-open parent with
/// `O_NOFOLLOW`; the returned descriptor is therefore anchored to the checked
/// session tree even if another process renames or replaces a parent path
/// while the read is starting. Callers must verify and consume this same
/// descriptor rather than reopening the returned display path.
pub fn open_session_artifact_for_read(session_id: &str, relative_path: &Path) -> io::Result<File> {
    if !is_valid_session_id(session_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid artifact session",
        ));
    }
    validate_artifact_relative_path(relative_path)?;
    let sessions_root = artifact_sessions_root()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid artifact root"))?;
    let root_metadata = std::fs::symlink_metadata(&sessions_root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact sessions root must be a real directory",
        ));
    }
    // macOS commonly exposes `/var` as a system symlink to `/private/var`.
    // Resolve that trusted ancestor spelling once, then descriptor-traverse
    // every session-owned component without following links.
    let path = sessions_root
        .canonicalize()?
        .join(session_id)
        .join(relative_path);

    #[cfg(unix)]
    {
        open_absolute_regular_file_no_follow(&path)
    }
    #[cfg(not(unix))]
    {
        let resolved = resolve_session_artifact_for_read(session_id, relative_path)?;
        if std::fs::symlink_metadata(&resolved)?
            .file_type()
            .is_symlink()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact symlink refused",
            ));
        }
        let file = File::open(resolved)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact is not a regular file",
            ));
        }
        Ok(file)
    }
}

/// Clone retained artifacts into a fork's own session namespace.
///
/// Fork transcripts preserve their opaque handle names. Copying the canonical
/// files under the child session keeps those names locally resolvable without
/// weakening current-session isolation or retaining a mutable link to the
/// parent directory.
pub fn clone_artifact_records_for_session(
    records: &[ArtifactRecord],
    source_session_id: &str,
    target_session_id: &str,
) -> io::Result<Vec<ArtifactRecord>> {
    records
        .iter()
        .map(|record| {
            clone_artifact_record_for_session(record, source_session_id, target_session_id)
        })
        .collect()
}

fn clone_artifact_record_for_session(
    record: &ArtifactRecord,
    source_session_id: &str,
    target_session_id: &str,
) -> io::Result<ArtifactRecord> {
    let mut source = open_artifact_record_source(record, source_session_id)?;
    let extension = record
        .storage_path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("txt");
    let relative_path = session_artifact_relative_path_with_extension(&record.id, extension)?;
    let expected_sha = adaptive_sha_from_artifact_id(&record.id);
    let expected_size = (record.byte_size != 0).then_some(record.byte_size);
    let (_, byte_size, _) = persist_session_artifact_reader(
        target_session_id,
        &relative_path,
        &mut source,
        expected_sha.as_deref(),
        expected_size,
    )?;

    let mut cloned = record.clone();
    cloned.session_id = target_session_id.to_string();
    cloned.storage_path = relative_path;
    cloned.byte_size = byte_size;
    Ok(cloned)
}

fn open_artifact_record_source(
    record: &ArtifactRecord,
    source_session_id: &str,
) -> io::Result<File> {
    if !is_valid_session_id(source_session_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid fork source artifact session",
        ));
    }
    let owning_session = if record.session_id.trim().is_empty() {
        // ArtifactRecord::session_id was added after the first persisted
        // artifact format. The SavedSession owner is authoritative for this
        // one legacy shape; a non-empty mismatch must never be reinterpreted.
        source_session_id
    } else if record.session_id == source_session_id {
        record.session_id.as_str()
    } else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact record belongs to a different source session",
        ));
    };

    if record.storage_path.is_relative() {
        return open_session_artifact_for_read(owning_session, &record.storage_path);
    }

    open_retained_absolute_artifact_for_read(owning_session, &record.storage_path)
}

/// Open an absolute compatibility artifact only when it remains inside the
/// owning session's artifact directory.
///
/// Imported/tampered SavedSession metadata is untrusted. In particular, an
/// absolute record must never turn Alt+V into a general local-file reader.
pub(crate) fn open_retained_absolute_artifact_for_read(
    session_id: &str,
    path: &Path,
) -> io::Result<File> {
    if !is_valid_session_id(session_id) || !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid absolute artifact owner or path",
        ));
    }
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "absolute artifact symlink refused",
        ));
    }
    let canonical = path.canonicalize()?;
    let inside_session = artifact_sessions_root()
        .and_then(|root| root.canonicalize().ok())
        .is_some_and(|root| canonical.starts_with(root.join(session_id).join(ARTIFACTS_DIR_NAME)));
    // The historical global `tool_outputs` directory was not namespaced by
    // session and its ArtifactRecords carried no trustworthy digest. Restored
    // absolute records from that root therefore cannot be authorized safely;
    // live classic-mode details remain available in-process, while resume
    // marks those old records unavailable instead of crossing session bounds.
    if !inside_session {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact record source is outside retained Codewhale storage",
        ));
    }
    #[cfg(unix)]
    {
        open_absolute_regular_file_no_follow(&canonical)
    }
    #[cfg(not(unix))]
    {
        let file = File::open(canonical)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artifact record source is not a regular file",
            ));
        }
        Ok(file)
    }
}

#[must_use]
pub fn adaptive_sha_from_artifact_id(artifact_id: &str) -> Option<String> {
    let sha = artifact_id.strip_prefix("art_output_")?.split('_').next()?;
    (sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| sha.to_ascii_lowercase())
}

#[cfg(unix)]
fn open_absolute_regular_file_no_follow(path: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;

    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact path must be absolute",
        ));
    }
    let root = CString::new("/").expect("static root has no NUL");
    // SAFETY: `root` is a valid C string and these flags require no mode.
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `root_fd` is a fresh descriptor owned by this function.
    let mut current = unsafe { File::from_raw_fd(root_fd) };
    let mut components = path
        .components()
        .filter_map(|component| match component {
            Component::RootDir => None,
            Component::Normal(component) => Some(Ok(component)),
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                Some(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact path is not normalized",
                )))
            }
        })
        .peekable();
    let mut opened_leaf = false;
    while let Some(component) = components.next() {
        let component = component?;
        let component = CString::new(component.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact path contains a NUL byte",
            )
        })?;
        let leaf = components.peek().is_none();
        #[cfg(test)]
        if leaf
            && let Some(hook) = BEFORE_SESSION_ARTIFACT_LEAF_OPEN_HOOK
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
        {
            hook();
        }
        let flags = if leaf {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY
        };
        // SAFETY: `current` is an open directory descriptor, `component` is
        // NUL-terminated, and the flags require no variadic mode.
        let fd = unsafe { libc::openat(current.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor returned by `openat`.
        current = unsafe { File::from_raw_fd(fd) };
        opened_leaf = leaf;
    }
    if !opened_leaf {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact path must name a file",
        ));
    }
    let metadata = current.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact must be a singly linked regular file",
        ));
    }
    Ok(current)
}

#[cfg(unix)]
fn open_absolute_directory_no_follow(path: &Path) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact directory path must be absolute",
        ));
    }
    let root = CString::new("/").expect("static root has no NUL");
    // SAFETY: `root` is a valid C string and these flags require no mode.
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `root_fd` is a fresh descriptor owned by this function.
    let mut current = unsafe { File::from_raw_fd(root_fd) };
    for component in path.components() {
        let component = match component {
            Component::RootDir => continue,
            Component::Normal(component) => component,
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact directory path is not normalized",
                ));
            }
        };
        let component = CString::new(component.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact directory path contains a NUL byte",
            )
        })?;
        // SAFETY: `current` is an open directory descriptor and `component`
        // is a NUL-terminated path segment.
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor returned by `openat`.
        current = unsafe { File::from_raw_fd(fd) };
    }
    if !current.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact path is not a directory",
        ));
    }
    Ok(current)
}

#[cfg(unix)]
fn artifact_write_anchor(sessions_root: &Path) -> io::Result<(PathBuf, PathBuf)> {
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = sessions_root.strip_prefix(&home)
        && !relative.as_os_str().is_empty()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Ok((home.canonicalize()?, relative.to_path_buf()));
    }
    let parent = sessions_root.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact sessions root has no trusted parent",
        )
    })?;
    let leaf = sessions_root.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact sessions root has no directory name",
        )
    })?;
    Ok((parent.canonicalize()?, PathBuf::from(leaf)))
}

#[cfg(unix)]
fn open_or_create_directory_at(parent: &File, component: &std::ffi::OsStr) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let component = CString::new(component.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact directory component contains a NUL byte",
        )
    })?;
    let open = || {
        // SAFETY: `parent` is an open directory descriptor and `component`
        // is a NUL-terminated path segment.
        unsafe {
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        }
    };
    let mut fd = open();
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(error);
        }
        // SAFETY: the parent descriptor and component are valid; mode 0700
        // keeps newly created artifact directories private even with umask 0.
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) };
        if created < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error);
            }
        }
        fd = open();
    }
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor returned by `openat`.
    let directory = unsafe { File::from_raw_fd(fd) };
    if !directory.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact path component is not a directory",
        ));
    }
    Ok(directory)
}

#[cfg(unix)]
struct PendingSessionArtifactWrite {
    file: File,
    directory: File,
    temporary_name: std::ffi::CString,
    target_name: std::ffi::CString,
    absolute_path: PathBuf,
    renamed: bool,
}

#[cfg(unix)]
impl PendingSessionArtifactWrite {
    fn new(session_id: &str, relative_path: &Path) -> io::Result<Self> {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::ffi::OsStrExt as _;

        if !is_valid_session_id(session_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid artifact session",
            ));
        }
        validate_artifact_relative_path(relative_path)?;
        let sessions_root = artifact_sessions_root().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve artifact sessions root",
            )
        })?;
        let (anchor, root_relative) = artifact_write_anchor(&sessions_root)?;
        let mut directory = open_absolute_directory_no_follow(&anchor)?;
        for component in root_relative
            .components()
            .chain(std::iter::once(Component::Normal(session_id.as_ref())))
            .chain(
                relative_path
                    .parent()
                    .into_iter()
                    .flat_map(Path::components),
            )
        {
            let Component::Normal(component) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid artifact directory component",
                ));
            };
            directory = open_or_create_directory_at(&directory, component)?;
        }

        let target = relative_path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact path has no file name",
            )
        })?;
        let target_name = CString::new(target.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact file name contains a NUL byte",
            )
        })?;
        for _ in 0..32 {
            let temporary_name = CString::new(format!(
                ".{}.{}.tmp",
                target.to_string_lossy(),
                uuid::Uuid::new_v4().simple()
            ))
            .expect("generated artifact temp name has no NUL");
            // SAFETY: the directory descriptor and generated C string are
            // valid. O_EXCL prevents clobbering an attacker-created name and
            // mode 0600 keeps exact output private independent of umask.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    temporary_name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW,
                    0o600,
                )
            };
            if fd >= 0 {
                // SAFETY: `fd` is a fresh descriptor returned by `openat`.
                let file = unsafe { File::from_raw_fd(fd) };
                return Ok(Self {
                    file,
                    directory,
                    temporary_name,
                    target_name,
                    absolute_path: sessions_root.join(session_id).join(relative_path),
                    renamed: false,
                });
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a private artifact temp file",
        ))
    }

    fn commit(mut self, session_id: &str, relative_path: &Path) -> io::Result<PathBuf> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::MetadataExt as _;

        self.file.sync_all()?;
        #[cfg(test)]
        if let Some(hook) = BEFORE_SESSION_ARTIFACT_RENAME_HOOK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            hook();
        }
        // SAFETY: both names are single NUL-terminated components relative to
        // the same held directory descriptor. This cannot follow a swapped
        // pathname outside that directory.
        let renamed = unsafe {
            libc::renameat(
                self.directory.as_raw_fd(),
                self.temporary_name.as_ptr(),
                self.directory.as_raw_fd(),
                self.target_name.as_ptr(),
            )
        };
        if renamed < 0 {
            return Err(io::Error::last_os_error());
        }
        self.renamed = true;
        self.directory.sync_all()?;

        // The directory may have been renamed after it was opened. Reopen the
        // public session-relative name and compare identities before reporting
        // success; if the namespace binding changed, remove the anchored copy
        // and force the broker's truthful inline fallback.
        let written = self.file.metadata()?;
        let binding_matches = open_session_artifact_for_read(session_id, relative_path)
            .and_then(|file| file.metadata())
            .is_ok_and(|current| current.dev() == written.dev() && current.ino() == written.ino());
        if !binding_matches {
            // SAFETY: target_name remains a single component under the held
            // artifact directory. Failure to unlink is secondary to the
            // fail-closed write result and cannot touch the swapped namespace.
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), self.target_name.as_ptr(), 0);
            }
            let _ = self.directory.sync_all();
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact namespace changed during atomic write",
            ));
        }
        Ok(self.absolute_path.clone())
    }
}

#[cfg(unix)]
impl Drop for PendingSessionArtifactWrite {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd as _;
        if self.renamed {
            return;
        }
        // SAFETY: temporary_name is a single component created beneath this
        // held directory descriptor. Best-effort cleanup cannot escape it.
        unsafe {
            libc::unlinkat(self.directory.as_raw_fd(), self.temporary_name.as_ptr(), 0);
        }
    }
}

#[cfg(unix)]
fn persist_session_artifact_reader(
    session_id: &str,
    relative_path: &Path,
    source: &mut impl Read,
    expected_sha256: Option<&str>,
    expected_size: Option<u64>,
) -> io::Result<(PathBuf, u64, String)> {
    let mut pending = PendingSessionArtifactWrite::new(session_id, relative_path)?;
    let mut hasher = Sha256::new();
    let mut byte_size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        pending.file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        byte_size = byte_size.saturating_add(read as u64);
    }
    let sha256 = crate::hashing::hex_bytes(hasher.finalize());
    if expected_sha256.is_some_and(|expected| sha256 != expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact source failed its content-addressed digest",
        ));
    }
    if expected_size.is_some_and(|expected| byte_size != expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact source size no longer matches its record",
        ));
    }
    let absolute_path = pending.commit(session_id, relative_path)?;
    Ok((absolute_path, byte_size, sha256))
}

#[cfg(not(unix))]
fn persist_session_artifact_reader(
    session_id: &str,
    relative_path: &Path,
    source: &mut impl Read,
    expected_sha256: Option<&str>,
    expected_size: Option<u64>,
) -> io::Result<(PathBuf, u64, String)> {
    let absolute_path = secure_session_artifact_path(session_id, relative_path, true)?;
    let parent = absolute_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact destination has no parent",
        )
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut hasher = Sha256::new();
    let mut byte_size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        temporary.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        byte_size = byte_size.saturating_add(read as u64);
    }
    let sha256 = crate::hashing::hex_bytes(hasher.finalize());
    if expected_sha256.is_some_and(|expected| sha256 != expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact source failed its content-addressed digest",
        ));
    }
    if expected_size.is_some_and(|expected| byte_size != expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact source size no longer matches its record",
        ));
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(&absolute_path)
        .map_err(|error| error.error)?;
    Ok((absolute_path, byte_size, sha256))
}

#[cfg(all(test, unix))]
pub(crate) fn set_before_session_artifact_leaf_open_hook(hook: impl FnOnce() + Send + 'static) {
    *BEFORE_SESSION_ARTIFACT_LEAF_OPEN_HOOK
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(Box::new(hook));
}

#[cfg(all(test, unix))]
pub(crate) fn set_before_session_artifact_rename_hook(hook: impl FnOnce() + Send + 'static) {
    *BEFORE_SESSION_ARTIFACT_RENAME_HOOK
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(Box::new(hook));
}

pub fn write_session_artifact(
    session_id: &str,
    artifact_id: &str,
    content: &str,
) -> io::Result<(PathBuf, PathBuf)> {
    let relative_path = session_artifact_relative_path(artifact_id);
    let mut source = std::io::Cursor::new(content.as_bytes());
    let (absolute_path, _, _) =
        persist_session_artifact_reader(session_id, &relative_path, &mut source, None, None)?;
    Ok((absolute_path, relative_path))
}

/// Write arbitrary fetched bytes into a session artifact with a validated
/// extension. Media fetches use this after magic-byte validation.
pub fn write_session_artifact_bytes(
    session_id: &str,
    artifact_id: &str,
    extension: &str,
    content: &[u8],
) -> io::Result<(PathBuf, PathBuf)> {
    let relative_path = session_artifact_relative_path_with_extension(artifact_id, extension)?;
    let mut source = std::io::Cursor::new(content);
    let (absolute_path, _, _) =
        persist_session_artifact_reader(session_id, &relative_path, &mut source, None, None)?;
    Ok((absolute_path, relative_path))
}

fn preview_text(content: &str, max_chars: usize) -> String {
    let mut preview: String = content.chars().take(max_chars).collect();
    if content.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

pub fn record_tool_output_artifact(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    storage_path: impl Into<PathBuf>,
    content: &str,
) -> ArtifactRecord {
    let storage_path = storage_path.into();
    let byte_size = std::fs::metadata(&storage_path)
        .map(|metadata| metadata.len())
        .unwrap_or_else(|_| content.len() as u64);
    record_tool_output_artifact_with_size(
        session_id,
        tool_call_id,
        tool_name,
        storage_path,
        byte_size,
        &preview_text(content, 200),
    )
}

pub fn record_tool_output_artifact_with_size(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    storage_path: impl Into<PathBuf>,
    byte_size: u64,
    preview: &str,
) -> ArtifactRecord {
    ArtifactRecord {
        id: artifact_id_for_tool_call(tool_call_id),
        kind: ArtifactKind::ToolOutput,
        session_id: session_id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        success: None,
        created_at: Utc::now(),
        byte_size,
        preview: preview_text(preview, 200),
        storage_path: storage_path.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptArtifactRef {
    pub artifact_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub byte_size: u64,
    pub storage_path: PathBuf,
    pub preview: String,
}

impl From<&ArtifactRecord> for TranscriptArtifactRef {
    fn from(record: &ArtifactRecord) -> Self {
        Self {
            artifact_id: record.id.clone(),
            tool_name: record.tool_name.clone(),
            tool_call_id: record.tool_call_id.clone(),
            byte_size: record.byte_size,
            storage_path: record.storage_path.clone(),
            preview: record.preview.clone(),
        }
    }
}

#[must_use]
pub fn render_transcript_artifact_ref(reference: &TranscriptArtifactRef) -> String {
    // The model sees several identifiers in this block. Keep a literal
    // retrieve command next to them so it does not have to infer which
    // field is accepted by `retrieve_tool_result`.
    format!(
        "[artifact: {tool}]\n\
         id:           {id}\n\
         tool:         {tool}\n\
         tool_call_id: {tool_call_id}\n\
         size:         {size}\n\
         path:         {path}\n\
         preview:      {preview}\n\
         retrieve:     retrieve_tool_result ref={id}",
        tool = reference.tool_name,
        id = reference.artifact_id,
        tool_call_id = reference.tool_call_id,
        size = format_byte_size(reference.byte_size),
        path = format_artifact_relative_path(&reference.storage_path),
        preview = reference.preview.replace('\n', " "),
    )
}

#[must_use]
pub fn format_artifact_relative_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

#[must_use]
pub fn format_byte_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        format!("{} MB", bytes.div_ceil(MIB))
    } else if bytes >= KIB {
        format!("{} KB", bytes.div_ceil(KIB))
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestArtifactSessionsRoot {
        prior: Option<PathBuf>,
    }

    impl Drop for TestArtifactSessionsRoot {
        fn drop(&mut self) {
            set_test_artifact_sessions_root(self.prior.take());
        }
    }

    fn set_test_sessions_root(root: PathBuf) -> TestArtifactSessionsRoot {
        TestArtifactSessionsRoot {
            prior: set_test_artifact_sessions_root(Some(root)),
        }
    }

    #[test]
    fn transcript_ref_renders_relative_paths_with_forward_slashes() {
        let reference = TranscriptArtifactRef {
            artifact_id: "art_call-big".to_string(),
            tool_name: "exec_shell".to_string(),
            tool_call_id: "call-big".to_string(),
            byte_size: 1024,
            storage_path: PathBuf::from(r"artifacts\art_call-big.txt"),
            preview: "checking crate".to_string(),
        };

        let rendered = render_transcript_artifact_ref(&reference);

        assert!(rendered.contains("path:         artifacts/art_call-big.txt"));
        assert!(
            rendered.contains("retrieve:     retrieve_tool_result ref=art_call-big"),
            "rendered block must embed the literal retrieve command: {rendered}"
        );
    }

    #[test]
    fn session_artifact_absolute_path_uses_test_sessions_root() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));

        let path = session_artifact_absolute_path(
            "session-123",
            &PathBuf::from("artifacts").join("art_call-big.txt"),
        )
        .expect("path");

        assert_eq!(
            path,
            tmp.path()
                .join("sessions")
                .join("session-123")
                .join("artifacts")
                .join("art_call-big.txt")
        );
    }

    #[test]
    fn binary_session_artifact_uses_validated_extension_and_exact_bytes() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));
        let bytes = b"\x89PNG\r\n\x1a\nfixture";

        let (absolute, relative) =
            write_session_artifact_bytes("session-123", "web/media", ".PNG", bytes)
                .expect("write binary artifact");

        assert_eq!(relative, PathBuf::from("artifacts/web_media.png"));
        assert_eq!(std::fs::read(absolute).unwrap(), bytes);
        assert!(write_session_artifact_bytes("session-123", "bad", "../png", bytes).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn write_refuses_symlinked_session_or_artifact_parent() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("artifacts")).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        let _root = set_test_sessions_root(sessions.clone());

        std::os::unix::fs::symlink(&outside, sessions.join("linked-session")).unwrap();
        assert!(write_session_artifact("linked-session", "secret", "nope").is_err());
        assert!(!outside.join("artifacts/art_secret.txt").exists());

        std::fs::create_dir(sessions.join("normal-session")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("artifacts"),
            sessions.join("normal-session/artifacts"),
        )
        .unwrap();
        assert!(write_session_artifact("normal-session", "secret", "nope").is_err());
        assert!(!outside.join("artifacts/art_secret.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn write_refuses_symlinked_sessions_root() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside-sessions");
        std::fs::create_dir_all(&outside).unwrap();
        let linked_root = tmp.path().join("sessions-link");
        std::os::unix::fs::symlink(&outside, &linked_root).unwrap();
        let _root = set_test_sessions_root(linked_root);

        assert!(write_session_artifact("session-123", "secret", "nope").is_err());
        assert!(
            !outside
                .join("session-123/artifacts/art_secret.txt")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_does_not_follow_a_swapped_artifact_parent() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let session_artifacts = sessions.join("attacker-session/artifacts");
        let outside_artifacts = tmp.path().join("outside/artifacts");
        std::fs::create_dir_all(&session_artifacts).unwrap();
        std::fs::create_dir_all(&outside_artifacts).unwrap();
        let outside_target = outside_artifacts.join("art_evidence.txt");
        std::fs::write(&outside_target, "OWNER_SECRET_MUST_NOT_CHANGE").unwrap();
        let _root = set_test_sessions_root(sessions);
        let session_artifacts_for_swap = session_artifacts.clone();
        let outside_artifacts_for_swap = outside_artifacts.clone();
        set_before_session_artifact_rename_hook(move || {
            let original = session_artifacts_for_swap.with_file_name("artifacts-original");
            std::fs::rename(&session_artifacts_for_swap, &original).unwrap();
            std::os::unix::fs::symlink(&outside_artifacts_for_swap, &session_artifacts_for_swap)
                .unwrap();
        });

        let error = write_session_artifact(
            "attacker-session",
            "evidence",
            "CW_MUST_STAY_IN_SESSION_STORAGE",
        )
        .expect_err("namespace swap must fail the write closed");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::read_to_string(outside_target).unwrap(),
            "OWNER_SECRET_MUST_NOT_CHANGE"
        );
        let original = session_artifacts.with_file_name("artifacts-original");
        assert!(!original.join("art_evidence.txt").exists());
        assert!(std::fs::read_dir(original).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn read_refuses_cross_session_artifact_parent_symlink() {
        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let owner_artifacts = sessions.join("owner/artifacts");
        std::fs::create_dir_all(&owner_artifacts).unwrap();
        std::fs::write(owner_artifacts.join("art_output.txt"), "owner secret").unwrap();
        std::fs::create_dir_all(sessions.join("attacker")).unwrap();
        std::os::unix::fs::symlink(&owner_artifacts, sessions.join("attacker/artifacts")).unwrap();
        let _root = set_test_sessions_root(sessions);

        let error =
            resolve_session_artifact_for_read("attacker", Path::new("artifacts/art_output.txt"))
                .expect_err("cross-session parent symlink must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn fork_clone_uses_saved_session_owner_only_for_legacy_empty_records() {
        use std::io::Read as _;

        let _guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));
        let raw = "legacy exact fork evidence\n";
        let sha = crate::hashing::sha256_hex(raw.as_bytes());
        let artifact_id = format!("art_output_{sha}_0123456789ab");
        let (_, relative_path) =
            write_session_artifact("source-session", &artifact_id, raw).unwrap();
        let legacy = ArtifactRecord {
            id: artifact_id,
            kind: ArtifactKind::ToolOutput,
            session_id: String::new(),
            tool_call_id: "call-legacy".to_string(),
            tool_name: "run_tests".to_string(),
            success: Some(false),
            created_at: Utc::now(),
            byte_size: raw.len() as u64,
            preview: "legacy exact fork evidence".to_string(),
            storage_path: relative_path,
        };

        let cloned = clone_artifact_records_for_session(
            std::slice::from_ref(&legacy),
            "source-session",
            "child-session",
        )
        .expect("legacy empty owner inherits from SavedSession");
        assert_eq!(cloned[0].session_id, "child-session");
        let mut child =
            open_session_artifact_for_read("child-session", &cloned[0].storage_path).unwrap();
        let mut copied = String::new();
        child.read_to_string(&mut copied).unwrap();
        assert_eq!(copied, raw);

        let mut mismatched = legacy;
        mismatched.session_id = "other-session".to_string();
        let error =
            clone_artifact_records_for_session(&[mismatched], "source-session", "another-child")
                .expect_err("non-empty cross-session ownership must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn restored_absolute_record_cannot_cross_into_global_legacy_spill_storage() {
        let _artifact_guard = TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _spill_guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _root = set_test_sessions_root(tmp.path().join("sessions"));
        let spill_root = tmp.path().join("tool_outputs");
        std::fs::create_dir_all(&spill_root).unwrap();
        let prior_spill = crate::tools::truncate::set_test_spillover_root(Some(spill_root.clone()));
        struct SpillReset(Option<PathBuf>);
        impl Drop for SpillReset {
            fn drop(&mut self) {
                crate::tools::truncate::set_test_spillover_root(self.0.take());
            }
        }
        let _spill_reset = SpillReset(prior_spill);
        let other_record = spill_root.join("call-other-session.txt");
        std::fs::write(&other_record, "CW_OTHER_SESSION_LEGACY_SECRET").unwrap();

        let error = open_retained_absolute_artifact_for_read("resume-session", &other_record)
            .expect_err("global legacy spill files are not session-owned on resume");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}

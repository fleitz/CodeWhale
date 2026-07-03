//! Low-level process management: platform-specific child process spawning,
//! process group killing, parent-death signaling, Windows job objects,
//! and reader-thread helpers.

use std::io::{Read, Write};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(windows)]
use super::types::WindowsJob;
use super::types::{ShellChild, ShellExitStatus, StdinWriter};

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use windows::core::PCWSTR;

pub(crate) const SYNC_READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const STALE_NO_OUTPUT_AFTER: Duration = Duration::from_secs(60);

#[cfg(unix)]
pub(crate) fn kill_child_process_group(child: &mut Child) -> std::io::Result<()> {
    let pgid = child.id() as libc::pid_t;
    if pgid <= 0 {
        return child.kill();
    }

    let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            child.kill()
        }
    }
}

/// Configure parent-death signaling so shell-spawned children are reaped when
/// the TUI dies abnormally (#421). On Linux this installs
/// `PR_SET_PDEATHSIG(SIGTERM)` via `pre_exec` — the kernel then sends SIGTERM
/// to the child the moment the parent process exits, even on SIGKILL of the
/// TUI. The cancellation path already SIGKILLs the whole process group, so
/// this only fires when the parent dies without running its drop / cleanup
/// code (panic during shutdown, OOM, hardware crash, etc.).
///
/// On macOS / Windows there's no kernel equivalent. The existing graceful
/// path (`kill_child_process_group` from the cancellation token) still
/// handles normal shutdown; abnormal exit can leak children — tracked as a
/// follow-up watchdog item per the original issue's acceptance criteria.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
pub(crate) fn install_parent_death_signal(cmd: &mut Command) {
    // SAFETY: `pre_exec` runs in the child between fork and exec. The closure
    // only calls `libc::prctl` with stack-allocated constant arguments and
    // does not touch heap memory or the parent's locks. Both requirements
    // (async-signal-safe + no allocation in the post-fork window) are met.
    unsafe {
        cmd.pre_exec(|| {
            let result = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
            if result == -1 {
                // Surface the errno but do not abort the spawn — the child
                // will simply lose the parent-death cleanup safety net.
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// Attach `args` to a `std::process::Command`, honoring shell-quoting on
/// Windows.
///
/// Issue #1691: on Windows the shell command is invoked as
/// `cmd /C "chcp 65001 >NUL & <command>"`. Rust's `Command::arg` applies
/// MSVCRT (`CommandLineToArgvW`) escaping, turning the embedded `"` in a
/// quoted argument (e.g. `git commit -m "feat: complete sub-pages"`) into
/// `\"`. `cmd.exe` does NOT use MSVCRT parsing — it treats `\` literally and
/// `"` as a bare quote toggle — so the escaped payload is mis-tokenized and
/// `git` receives `feat:`, `complete`, `sub-pages"` as separate pathspecs
/// (the reported `pathspec 'sub-pages"' did not match` symptom). Passing the
/// `cmd /C` payload through `CommandExt::raw_arg` suppresses std's escaping so
/// the string reaches `cmd.exe` verbatim, exactly as a terminal would.
#[cfg(windows)]
pub(crate) fn push_shell_args(cmd: &mut Command, program: &str, args: &[String]) {
    use std::os::windows::process::CommandExt;
    // The `cmd /C <payload>` shape is the only place std's per-arg escaping
    // corrupts a quoted command. Pass `/C` and the payload raw so the quotes
    // survive; any other program keeps normal (correct) escaping. Match `cmd`
    // by file stem so a full path (`C:\Windows\System32\cmd.exe`) or `.exe`
    // suffix still triggers the raw-arg path.
    let is_cmd = std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("cmd"))
        .unwrap_or(false);
    if is_cmd && args.len() == 2 && args[0].eq_ignore_ascii_case("/C") {
        cmd.raw_arg(&args[0]);
        cmd.raw_arg(&args[1]);
    } else {
        cmd.args(args);
    }
}

#[cfg(not(windows))]
pub(crate) fn push_shell_args(cmd: &mut Command, _program: &str, args: &[String]) {
    // Unix delegates tokenization entirely to `sh -c <command>`; the command
    // string is passed as a single argv entry and never split by us.
    cmd.args(args);
}

#[cfg(not(all(target_os = "linux", not(target_env = "ohos"))))]
pub(crate) fn install_parent_death_signal(_cmd: &mut Command) {
    // No kernel-level equivalent on macOS / Windows. The cooperative
    // cancellation + process_group SIGKILL path covers normal shutdown;
    // abnormal exit (panic without unwind, SIGKILL of the TUI) can still
    // leak children on those platforms — tracked as a follow-up.
}

#[cfg(windows)]
// SAFETY: Windows job handles are process-wide kernel handles. Moving the
// wrapper between threads does not invalidate the handle, and access is
// externally synchronized by ShellManager's mutex.
unsafe impl Send for WindowsJob {}
#[cfg(windows)]
// SAFETY: The wrapper exposes only terminate/drop operations around a kernel
// handle; concurrent use is guarded by ShellManager.
unsafe impl Sync for WindowsJob {}

#[cfg(windows)]
impl WindowsJob {
    pub(crate) fn attach_to_child(child: &Child) -> std::io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()).map_err(windows_io_error)? };
        let job = Self { handle };

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(windows_io_error)?;

            let process_handle = HANDLE(child.as_raw_handle());
            AssignProcessToJobObject(job.handle, process_handle).map_err(windows_io_error)?;
        }

        Ok(job)
    }

    pub(crate) fn terminate(&self) -> std::io::Result<()> {
        unsafe { TerminateJobObject(self.handle, 1).map_err(windows_io_error) }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn windows_io_error(error: windows::core::Error) -> std::io::Error {
    std::io::Error::other(error)
}

#[cfg(windows)]
pub(crate) fn terminate_windows_job(
    job: Option<&WindowsJob>,
    child: &mut Child,
) -> std::io::Result<()> {
    if let Some(job) = job {
        match job.terminate() {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "failed to terminate Windows job object; falling back to immediate child kill"
                );
            }
        }
    }
    child.kill()
}

#[cfg(windows)]
pub(crate) fn terminate_and_close_windows_job(windows_job: Option<WindowsJob>) {
    if let Some(job) = windows_job.as_ref()
        && let Err(err) = job.terminate()
    {
        tracing::warn!(
            ?err,
            "failed to terminate Windows shell job before closing job handle"
        );
    }
    drop(windows_job);
}

#[cfg(windows)]
pub(crate) fn terminate_child_and_close_windows_job(
    windows_job: Option<WindowsJob>,
    child: &mut Child,
) -> std::io::Result<()> {
    let result = terminate_windows_job(windows_job.as_ref(), child);
    drop(windows_job);
    result
}

#[cfg(windows)]
pub(crate) fn attach_windows_job(child: &Child, command: &str) -> Option<WindowsJob> {
    match WindowsJob::attach_to_child(child) {
        Ok(job) => Some(job),
        Err(error) => {
            tracing::warn!(
                ?error,
                command,
                "failed to attach Windows shell process to job object; descendant cleanup degraded"
            );
            None
        }
    }
}

impl ShellExitStatus {
    pub(crate) fn from_std(status: std::process::ExitStatus) -> Self {
        Self {
            code: status.code(),
            success: status.success(),
        }
    }

    #[cfg(not(target_env = "ohos"))]
    pub(crate) fn from_pty(status: portable_pty::ExitStatus) -> Self {
        let code = i32::try_from(status.exit_code()).unwrap_or(i32::MAX);
        Self {
            code: Some(code),
            success: status.success(),
        }
    }
}

impl ShellChild {
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ShellExitStatus>> {
        match self {
            ShellChild::Process(child) => child
                .try_wait()
                .map(|status| status.map(ShellExitStatus::from_std)),
            #[cfg(not(target_env = "ohos"))]
            ShellChild::Pty(child) => child
                .try_wait()
                .map(|status| status.map(ShellExitStatus::from_pty)),
        }
    }

    pub(crate) fn wait(&mut self) -> std::io::Result<ShellExitStatus> {
        match self {
            ShellChild::Process(child) => child.wait().map(ShellExitStatus::from_std),
            #[cfg(not(target_env = "ohos"))]
            ShellChild::Pty(child) => child.wait().map(ShellExitStatus::from_pty),
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn kill(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            ShellChild::Process(child) => kill_child_process_group(child),
            #[cfg(not(unix))]
            ShellChild::Process(child) => child.kill(),
            #[cfg(not(target_env = "ohos"))]
            ShellChild::Pty(child) => child.kill(),
        }
    }
}

impl StdinWriter {
    pub(crate) fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            StdinWriter::Pipe(stdin) => stdin.write_all(data),
            #[cfg(not(target_env = "ohos"))]
            StdinWriter::Pty(writer) => writer.write_all(data),
        }
    }

    pub(crate) fn flush(&mut self) -> std::io::Result<()> {
        match self {
            StdinWriter::Pipe(stdin) => stdin.flush(),
            #[cfg(not(target_env = "ohos"))]
            StdinWriter::Pty(writer) => writer.flush(),
        }
    }
}

pub(crate) fn spawn_reader_thread<R: Read + Send + 'static>(
    mut reader: R,
    buffer: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut guard) = buffer.lock() {
                        guard.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

pub(crate) fn spawn_sync_reader_thread<R: Read + Send + 'static>(
    mut reader: R,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        tx.send(buf).ok();
    });
    rx
}

pub(crate) fn recv_sync_reader_output(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    rx.recv_timeout(SYNC_READER_DRAIN_TIMEOUT)
        .unwrap_or_default()
}

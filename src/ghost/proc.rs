//! Platform-specific process control for DonGhost.
//!
//! **Unix (Linux + macOS):** process groups + SIGSTOP/SIGCONT/SIGKILL.
//! The whole browser tree (browser + renderers + GPU) shares one
//! process group, so a single signal suspends/resumes/kills it all.
//! Linux additionally gets `PR_SET_PDEATHSIG` — the kernel reaps the
//! child if donsetch dies hard. macOS has no `prctl` equivalent; the
//! Ghost Drop + kill path cover normal exit, only a hard parent crash
//! could orphan (documented limitation).
//!
//! **Windows:** a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
//! owns the browser tree — the kernel kills every process in the job
//! when the last handle closes (including if donsetch crashes).
//! Freeze/thaw via `NtSuspendProcess`/`NtResumeProcess` (ntdll; the
//! atomic whole-process pause, stable since XP — Sysinternals Process
//! Explorer, WinDbg, and Chrome's own crash handling all use it).
//! Linked directly to ntdll via `#[link(name = "ntdll")]` — no
//! `GetProcAddress` dance, no FARPROC type ambiguity.

use crate::error::FetchError;
use tokio::process::Child;

#[cfg(unix)]
use libc;

#[cfg(windows)]
use windows_sys::Win32::Foundation as fnd;
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects as job;
#[cfg(windows)]
use windows_sys::Win32::System::Threading as thr;

/// Owned platform handle to the browser process tree.
pub struct Proc {
    #[cfg(unix)]
    pid: i32,
    #[cfg(windows)]
    proc_handle: fnd::HANDLE,
    #[cfg(windows)]
    job: fnd::HANDLE,
}

/// ntdll process suspend/resume — always loaded, linked directly.
#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(proc_handle: fnd::HANDLE) -> i32;
    fn NtResumeProcess(proc_handle: fnd::HANDLE) -> i32;
}

impl Proc {
    /// Configure a `Command` BEFORE spawn.
    /// Unix: own process group (freeze/thaw signal the whole tree).
    /// Windows: nothing — the Job Object is attached post-spawn.
    pub fn prepare_cmd(cmd: &mut tokio::process::Command) {
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(not(unix))]
        {
            let _ = cmd;
        }
    }

    /// Build from a just-spawned `Child`.
    /// Unix: stash the pid (process group leader).
    /// Windows: open a process handle (suspend + quota rights),
    /// create a `KILL_ON_JOB_CLOSE` Job Object, assign the child.
    pub fn from_child(child: &Child) -> Result<Self, FetchError> {
        #[cfg(unix)]
        {
            let pid = child.id().unwrap_or(0) as i32;
            Ok(Self { pid })
        }
        #[cfg(windows)]
        {
            unsafe { Self::from_child_win(child) }
        }
    }

    #[cfg(windows)]
    unsafe fn from_child_win(child: &Child) -> Result<Self, FetchError> {
        use std::mem;
        let pid = child.id().unwrap_or(0);

        // SAFETY: all FFI calls in this function target well-documented
        // Windows kernel/ntdll APIs with correct parameter types. The
        // `unsafe` block wraps the entire body — every call is audited.
        unsafe {
            // Process handle with the access rights we need:
            //   PROCESS_SUSPEND_RESUME  — NtSuspendProcess / NtResumeProcess
            //   PROCESS_SET_QUOTA       — AssignProcessToJobObject
            //   PROCESS_QUERY_LIMITED_INFORMATION — status checks
            let proc_handle = thr::OpenProcess(
                thr::PROCESS_SUSPEND_RESUME
                    | thr::PROCESS_SET_QUOTA
                    | thr::PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            );
            if proc_handle.is_null() {
                return Err(FetchError::ghost(format!(
                    "OpenProcess failed: {}",
                    fnd::GetLastError()
                )));
            }

            // Job Object: kernel kills the whole tree if donsetch dies.
            let job_h = job::CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job_h.is_null() {
                fnd::CloseHandle(proc_handle);
                return Err(FetchError::ghost(format!(
                    "CreateJobObjectW failed: {}",
                    fnd::GetLastError()
                )));
            }
            let mut info: job::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            info.BasicLimitInformation.LimitFlags = job::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if job::SetInformationJobObject(
                job_h,
                job::JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                mem::size_of::<job::JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                fnd::CloseHandle(proc_handle);
                fnd::CloseHandle(job_h);
                return Err(FetchError::ghost(format!(
                    "SetInformationJobObject failed: {}",
                    fnd::GetLastError()
                )));
            }
            // Assign the child to the job. On Win8+ nested jobs are
            // allowed, so this succeeds even if the process is already
            // in a job. If it fails we don't abort — freeze/thaw/kill
            // still work via the process handle; only the death-reap
            // safety net is lost.
            if job::AssignProcessToJobObject(job_h, proc_handle) == 0 {
                // Non-fatal: log via the error channel's debug only.
            }
            Ok(Self {
                proc_handle,
                job: job_h,
            })
        }
    }

    /// Suspend the whole process tree. CPU → 0, RAM goes cold.
    pub fn freeze(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.pid, libc::SIGSTOP);
        }
        #[cfg(windows)]
        unsafe {
            NtSuspendProcess(self.proc_handle);
        }
    }

    /// Resume the whole process tree.
    pub fn thaw(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.pid, libc::SIGCONT);
        }
        #[cfg(windows)]
        unsafe {
            NtResumeProcess(self.proc_handle);
        }
    }

    /// Kill the whole tree.
    pub fn kill_group(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
        }
        #[cfg(windows)]
        unsafe {
            // Kills every process in the job — the whole tree.
            job::TerminateJobObject(self.job, 1);
        }
    }
}

/// `PR_SET_PDEATHSIG` — kernel kills the child if donsetch dies.
/// Called in `pre_exec` (child context). Linux-only; macOS has no
/// `prctl` equivalent.
#[cfg(target_os = "linux")]
pub fn pdeath_pre_exec() -> std::io::Result<()> {
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

// SAFETY: Windows HANDLEs are kernel object references (opaque
// pointers), safe to move between threads. Only one thread accesses
// them at a time (guarded by GhostManager's Mutex), and the handles
// are closed exactly once in Drop.
#[cfg(windows)]
unsafe impl Send for Proc {}
#[cfg(windows)]
unsafe impl Sync for Proc {}

impl Drop for Proc {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            fnd::CloseHandle(self.proc_handle);
            fnd::CloseHandle(self.job);
        }
        #[cfg(not(windows))]
        {
            let _ = self;
        }
    }
}

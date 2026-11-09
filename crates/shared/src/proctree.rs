//! A started program and everything it starts, ended together.
//!
//! On Unix the program leads its own process group. On Windows it runs in
//! a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: it is started
//! suspended, put in the job, then resumed, so nothing it starts (a
//! venv's `python.exe` redirector starts the base interpreter, which may
//! start more) can begin outside the job. Ending the tree, or dropping
//! it, ends every process still in it, which also closes every copy of
//! the program's output pipes, so a reader never waits for an end of file
//! that a left-behind grandchild holds open.
//!
//! Used for runs that are not confined by the launcher (whose own job
//! object does this): trusted environments, environment probes and
//! environment creation.

/// Set `cmd` up before it is spawned: its own process group on Unix,
/// started suspended on Windows (see [`Tree::adopt`], which must follow).
pub fn prepare(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = cmd;
}

/// A program started after [`prepare`], and everything it starts.
pub struct Tree {
    #[cfg(unix)]
    pgid: Option<i32>,
    #[cfg(windows)]
    job: Option<win::Handle>,
    /// Windows, when the job could not be made: the process id, ended
    /// with `taskkill /T`.
    #[cfg(windows)]
    fallback: Option<u32>,
}

impl Tree {
    /// Take charge of a program just spawned after [`prepare`]: `pid` is
    /// its id and, on Windows, `handle` its process handle. On Windows
    /// this puts it in a new job object and resumes it; if the job cannot
    /// be made it is resumed anyway and ended with `taskkill /T`.
    #[cfg(windows)]
    pub fn adopt(pid: u32, handle: std::os::windows::io::RawHandle) -> Tree {
        match win::job_for(handle) {
            Ok(job) => {
                win::resume(pid);
                Tree {
                    job: Some(job),
                    fallback: None,
                }
            }
            Err(_) => {
                win::resume(pid);
                Tree {
                    job: None,
                    fallback: Some(pid),
                }
            }
        }
    }

    /// Take charge of a program just spawned after [`prepare`]: it leads
    /// its own process group, `pid`.
    #[cfg(unix)]
    pub fn adopt(pid: u32) -> Tree {
        Tree {
            pgid: i32::try_from(pid).ok().filter(|p| *p > 1),
        }
    }

    /// Take charge of a `std` child spawned after [`prepare`].
    pub fn of(child: &std::process::Child) -> Tree {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            Tree::adopt(child.id(), child.as_raw_handle())
        }
        #[cfg(unix)]
        {
            Tree::adopt(child.id())
        }
    }

    /// End every process in the tree now. Safe to call more than once.
    pub fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        {
            if let Some(job) = &self.job {
                win::terminate(job);
            }
            if let Some(pid) = self.fallback {
                let _ = std::process::Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // Unix: anything left in the group. Windows: terminating the job
        // ends what is left in it (closing the handle would too).
        self.kill();
    }
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::RawHandle;
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    pub(super) struct Handle(HANDLE);
    // A job handle may be used from any thread.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn job_for(process: RawHandle) -> Result<Handle, u32> {
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job.is_null() {
            return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
        }
        let job = Handle(job);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        // No breakaway: a process in the job cannot start one outside it,
        // even one asking for its own job's silent breakaway (the venv
        // redirector's).
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 || unsafe { AssignProcessToJobObject(job.0, process as HANDLE) } == 0 {
            return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
        }
        Ok(job)
    }

    pub(super) fn terminate(job: &Handle) {
        unsafe { TerminateJobObject(job.0, 1) };
    }

    /// Resume every thread of a process started suspended (it has one).
    pub(super) fn resume(pid: u32) {
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snap == INVALID_HANDLE_VALUE || snap.is_null() {
            return;
        }
        let snap = Handle(snap);
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut more = unsafe { Thread32First(snap.0, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                let t = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !t.is_null() {
                    let t = Handle(t);
                    unsafe { ResumeThread(t.0) };
                }
            }
            more = unsafe { Thread32Next(snap.0, &mut entry) } != 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn python() -> Option<String> {
        ["python3", "python"]
            .into_iter()
            .map(String::from)
            .find(|p| {
                std::process::Command::new(p)
                    .args(["-c", "pass"])
                    .output()
                    .is_ok_and(|o| o.status.success())
            })
    }

    /// A program that leaves a grandchild holding its output pipe open:
    /// once the tree is ended, reading the output reaches its end at once.
    #[test]
    fn ending_the_tree_closes_a_grandchilds_copy_of_the_pipe() {
        let Some(py) = python() else {
            eprintln!("skipping: no Python");
            return;
        };
        let mut cmd = std::process::Command::new(&py);
        // The grandchild inherits stdout and sleeps; the child exits.
        cmd.args([
            "-c",
            "import subprocess, sys\n\
             subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'], close_fds=False)\n\
             print('started', flush=True)\n",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
        prepare(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let mut tree = Tree::of(&child);
        let mut out = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut out, &mut s);
            s
        });
        assert!(child.wait().unwrap().success());
        let started = Instant::now();
        tree.kill();
        let text = reader.join().unwrap();
        assert!(text.contains("started"), "{text}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the grandchild kept the pipe open"
        );
    }
}

//! Minimal Win32 surface for worker containment. Every `unsafe` block in the workspace
//! lives here; all other crates keep `#![forbid(unsafe_code)]`.
#![cfg_attr(not(windows), forbid(unsafe_code))]

#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_BASIC_UI_RESTRICTIONS,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::SystemServices::JOB_OBJECT_UILIMIT_ALL;

    /// Resource containment for one worker process, the Windows counterpart of the
    /// worker's own `RLIMIT_AS`/`PR_SET_PDEATHSIG` setup on Linux:
    /// - committed memory is capped per process,
    /// - the worker cannot spawn further processes,
    /// - it is killed when this handle closes, including when the GUI process dies,
    /// - crashes terminate instead of showing a Windows Error Reporting dialog,
    /// - it cannot touch other processes' windows, the clipboard or the desktop.
    ///
    /// This is not a filesystem or network sandbox.
    pub struct WorkerJob {
        _handle: OwnedHandle,
    }

    impl WorkerJob {
        pub fn contain(child: &Child, memory_limit: u64) -> io::Result<Self> {
            let memory_limit = usize::try_from(memory_limit)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "memory limit"))?;
            // SAFETY: both pointers may be null; the returned handle is checked and then
            // owned exactly once by `OwnedHandle`.
            let job = unsafe {
                let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                OwnedHandle::from_raw_handle(raw)
            };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            limits.ProcessMemoryLimit = memory_limit;
            set(&job, JobObjectExtendedLimitInformation, &limits)?;
            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: JOB_OBJECT_UILIMIT_ALL,
            };
            set(&job, JobObjectBasicUIRestrictions, &ui)?;
            // SAFETY: both handles are valid for the duration of the call.
            if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { _handle: job })
        }
    }

    fn set<T>(job: &OwnedHandle, class: i32, info: &T) -> io::Result<()> {
        let size = u32::try_from(size_of::<T>()).map_err(|_| io::Error::other("size"))?;
        // SAFETY: `info` points to a live, correctly sized `T` matching `class`.
        if unsafe {
            SetInformationJobObject(job.as_raw_handle(), class, (info as *const T).cast(), size)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use job::WorkerJob;

#[cfg(all(windows, test))]
mod tests {
    use super::WorkerJob;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn shell() -> std::process::Child {
        // An interactive cmd.exe blocks on stdin, so nothing runs before containment.
        Command::new("cmd.exe")
            .arg("/Q")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn contained_process_cannot_spawn_children() {
        let mut child = shell();
        let _job = WorkerJob::contain(&child, 1024 * 1024 * 1024).unwrap();
        let mut input = child.stdin.take().unwrap();
        input
            .write_all(
                b"set M=NCAT\r\necho %M%_ALIVE\r\nwhoami.exe >nul && echo %M%_SPAWNED\r\nexit\r\n",
            )
            .unwrap();
        drop(input);
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        child.wait().unwrap();
        assert!(output.contains("NCAT_ALIVE"), "{output}");
        assert!(!output.contains("NCAT_SPAWNED"), "{output}");
    }

    #[test]
    fn closing_the_job_kills_the_process() {
        let mut child = shell();
        let job = WorkerJob::contain(&child, 1024 * 1024 * 1024).unwrap();
        // Hold stdin open: `Child::wait` would close it and let cmd exit on its own.
        let _input = child.stdin.take().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "process survived job close");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

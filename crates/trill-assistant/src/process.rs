//! Starting the CLI so that it can always be stopped.
//!
//! The CLI starts helpers of its own, so killing only the process we started
//! would leave the rest running. Every child is therefore placed in a
//! [`ProcessTree`] before it executes a single instruction: on Windows a job
//! object that the kernel empties when its last handle closes, which also
//! covers Trill crashing; on Unix a process group, which has a keeper
//! beside it that ends it when Trill is gone.

use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Every process descended from one child, as a unit that can be killed.
pub(crate) struct ProcessTree {
    inner: imp::Tree,
}

impl ProcessTree {
    /// Kills every process in the tree, now. Safe to call repeatedly and
    /// after the processes have exited.
    pub(crate) fn kill(&self) {
        self.inner.kill();
    }

    /// Gives the tree up once its child has exited, and kills whatever the
    /// child left running (on Unix, where its keeper could be started).
    pub(crate) fn close(self) {}
}

/// Starts `command` with no console window and returns it together with the
/// tree it and its descendants belong to. Call [`ProcessTree::kill`] to stop
/// them; on Windows, dropping the tree also kills whatever is left in it.
pub(crate) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessTree)> {
    let (child, inner) = imp::spawn(command)?;
    Ok((child, ProcessTree { inner }))
}

/// Runs a command that is expected to print a little and exit, such as
/// `--version`, and returns what it printed. Its exit code is not reported:
/// the CLI's answers are judged by what they say. A command still running at
/// `timeout` is killed and reported as [`io::ErrorKind::TimedOut`].
pub(crate) fn run_captured(command: &mut Command, timeout: Duration) -> io::Result<String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let (mut child, tree) = spawn(command)?;
    let mut stdout = child.stdout.take().expect("stdout was piped");

    // Reading on another thread is what lets the wait below time out: a read
    // on a pipe cannot be interrupted, but killing the tree ends it.
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });

    match receiver.recv_timeout(timeout) {
        Ok(bytes) => {
            child.wait()?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        Err(_) => {
            tree.kill();
            let _ = child.wait();
            Err(io::Error::new(io::ErrorKind::TimedOut, "the command did not finish in time"))
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::mem;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    /// A job object set to kill its processes when the last handle to it
    /// closes. Trill holds the only handle, so the processes die when the
    /// tree is dropped and also when Trill itself is killed or crashes.
    pub(super) struct Tree {
        job: HANDLE,
    }

    // SAFETY: a job handle is a kernel object reference that any thread of the
    // owning process may use; the only calls made on it (assign, terminate,
    // close) are thread-safe kernel calls.
    unsafe impl Send for Tree {}
    // SAFETY: as above; `kill` takes `&self` and the kernel serializes access.
    unsafe impl Sync for Tree {}

    impl Tree {
        fn new() -> io::Result<Self> {
            // SAFETY: null attributes and a null name are documented as valid:
            // they ask for an unnamed job whose handle children do not inherit.
            let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let tree = Tree { job };

            // SAFETY: the all-zero bit pattern is a valid value for this
            // plain-data struct (integers only), and means "no limits".
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is a live, correctly sized struct of the type
            // that `JobObjectExtendedLimitInformation` requires, and `job` is
            // the valid handle created above.
            let set = unsafe {
                SetInformationJobObject(
                    tree.job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(tree)
        }

        pub(super) fn kill(&self) {
            // SAFETY: `job` stays valid until `drop`. Terminating a job with
            // no live processes is allowed and does nothing.
            unsafe { TerminateJobObject(self.job, 1) };
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: `job` was created by `new`, is closed nowhere else, and
            // is not used after this.
            unsafe { CloseHandle(self.job) };
        }
    }

    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Tree)> {
        let tree = Tree::new()?;
        // No window: a console program started by a GUI program otherwise
        // flashes one. Suspended: the child must be inside the job before it
        // runs, or a helper it starts first could escape the job.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn()?;

        let started = assign(&tree, &child).and_then(|()| resume(child.id()));
        if let Err(error) = started {
            // Still suspended or outside the job: do not leave it behind.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok((child, tree))
    }

    fn assign(tree: &Tree, child: &Child) -> io::Result<()> {
        // SAFETY: both handles are valid: the job for as long as `tree`
        // lives, the process for as long as `child` does.
        let assigned = unsafe { AssignProcessToJobObject(tree.job, child.as_raw_handle()) };
        if assigned == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Resumes the threads of a process created suspended.
    ///
    /// The standard library does not hand out the main thread's handle, so
    /// the threads are found through a snapshot instead.
    fn resume(process_id: u32) -> io::Result<()> {
        // SAFETY: a plain call; the result is checked before use.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: all-zero is a valid value for this plain-data struct.
        let mut entry: THREADENTRY32 = unsafe { mem::zeroed() };
        entry.dwSize = mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        // SAFETY: `snapshot` is valid and `entry` is a live struct whose
        // `dwSize` is set, as both calls require.
        let mut more = unsafe { Thread32First(snapshot, &raw mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == process_id {
                // SAFETY: a plain call; the result is checked before use.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    // SAFETY: `thread` is a valid handle opened with the right
                    // to resume, and is closed exactly once.
                    unsafe {
                        resumed |= ResumeThread(thread) != u32::MAX;
                        CloseHandle(thread);
                    }
                }
            }
            // SAFETY: as for `Thread32First`.
            more = unsafe { Thread32Next(snapshot, &raw mut entry) } != 0;
        }
        // SAFETY: `snapshot` is valid and not used after this.
        unsafe { CloseHandle(snapshot) };

        if resumed { Ok(()) } else { Err(io::Error::other("could not resume the new process")) }
    }
}

/// On Unix the group has a keeper: a shell in the group, which reads a pipe
/// that only Trill holds the other end of. However Trill ends (a crash, a
/// kill), the system closes that end, and the keeper ends the group and
/// itself with it. And as long as the keeper lives the group is not empty,
/// so its id is not given to another: killing it is safe at any time, and
/// closing or dropping the tree kills what the CLI left running, as on
/// Windows. Without a keeper (none could be started) it is the group alone.
#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, ChildStdin, Command, Stdio};

    unsafe extern "C" {
        /// POSIX `kill(2)`. Declared here to avoid a dependency for one call.
        fn kill(pid: i32, signal: i32) -> i32;
    }

    const SIGKILL: i32 = 9;
    /// What the keeper does: waits for its input to end, which it does only
    /// when Trill is gone, and then ends its group. (Trill never writes.)
    /// (`-s KILL`, as POSIX spells it: dash, Linux's usual `sh`, takes no
    /// `-KILL` before `--`.)
    const KEEPER: &str = "read _; kill -s KILL -- -\"$1\"";

    pub(super) struct Tree {
        group: i32,
        /// The keeper, and the end of its pipe that Trill holds.
        keeper: Option<(Child, ChildStdin)>,
    }

    impl Tree {
        pub(super) fn kill(&self) {
            // Only while the keeper holds the group open is its id certainly
            // still its own.
            if self.keeper.is_some() {
                // SAFETY: `kill` has no memory-safety requirements. A negative
                // pid addresses the group, which the keeper keeps alive.
                unsafe { kill(-self.group, SIGKILL) };
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            self.kill();
            if let Some((mut keeper, input)) = self.keeper.take() {
                drop(input);
                let _ = keeper.wait();
            }
        }
    }

    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Tree)> {
        command.process_group(0);
        let child = command.spawn()?;
        let group = i32::try_from(child.id()).map_err(io::Error::other)?;
        let keeper = Command::new("/bin/sh")
            .args(["-c", KEEPER, "keeper", &group.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(group)
            .spawn()
            .ok()
            .and_then(|mut keeper| Some((keeper.stdin.take()?, keeper)))
            .map(|(input, keeper)| (keeper, input));
        if keeper.is_none() {
            log::warn!("the assistant's CLI has no keeper: if Trill is killed, it is left to end by itself");
        }
        Ok((child, Tree { group, keeper }))
    }
}

//! Unix POSIX PTY backend (`posix_openpt` + `grantpt` + `unlockpt` + `fork`/`exec` + `TIOCSWINSZ`).

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use veloxdesk_core_traits::{CoreError, Result};

/// Running Unix PTY child process. Dropping kills the child and closes the master PTY.
pub struct PtyProcess {
    master: Arc<Mutex<Option<OwnedFd>>>,
    child_pid: Arc<Mutex<Option<libc::pid_t>>>,
}

pub struct PtyStreams {
    pub pty: PtyProcess,
    pub output: mpsc::UnboundedReceiver<Bytes>,
    pub exit: oneshot::Receiver<i32>,
}

impl PtyProcess {
    pub fn spawn(cols: u16, rows: u16, shell: &str) -> Result<PtyStreams> {
        let cols = cols.clamp(2, 1000);
        let rows = rows.clamp(2, 1000);

        // SAFETY: standard POSIX PTY master allocation.
        let master_raw = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master_raw < 0 {
            return Err(CoreError::Io(io::Error::last_os_error()));
        }
        // SAFETY: newly opened fd.
        let master = unsafe { OwnedFd::from_raw_fd(master_raw) };
        unsafe {
            if libc::grantpt(master.as_raw_fd()) != 0 || libc::unlockpt(master.as_raw_fd()) != 0 {
                return Err(CoreError::Io(io::Error::last_os_error()));
            }
        }
        let slave_name = unsafe {
            let ptr = libc::ptsname(master.as_raw_fd());
            if ptr.is_null() {
                return Err(CoreError::Io(io::Error::last_os_error()));
            }
            CStr::from_ptr(ptr).to_owned()
        };

        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        unsafe {
            let _ = libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const ws);
        }

        let cmd = if shell.trim().is_empty() {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        } else {
            shell.to_string()
        };
        let sh_c = CString::new("/bin/sh").expect("static");
        let dash_c = CString::new("-c").expect("static");
        let cmd_c = CString::new(cmd.as_str()).map_err(|_| CoreError::invalid("shell contains NUL"))?;
        let use_sh_c = cmd.contains(' ');

        // SAFETY: standard fork/exec PTY child setup.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(CoreError::Io(io::Error::last_os_error()));
        }
        if pid == 0 {
            unsafe {
                libc::setsid();
                let slave_fd = libc::open(slave_name.as_ptr(), libc::O_RDWR);
                if slave_fd < 0 {
                    libc::_exit(126);
                }
                let _ = libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0);
                libc::dup2(slave_fd, 0);
                libc::dup2(slave_fd, 1);
                libc::dup2(slave_fd, 2);
                if slave_fd > 2 {
                    libc::close(slave_fd);
                }
                libc::close(master.as_raw_fd());
                if use_sh_c {
                    let argv = [sh_c.as_ptr(), dash_c.as_ptr(), cmd_c.as_ptr(), std::ptr::null()];
                    libc::execvp(sh_c.as_ptr(), argv.as_ptr());
                } else {
                    let argv = [cmd_c.as_ptr(), std::ptr::null()];
                    libc::execvp(cmd_c.as_ptr(), argv.as_ptr());
                }
                libc::_exit(127);
            }
        }

        // Duplicate master fd for the dedicated reader thread.
        let read_raw = unsafe { libc::dup(master.as_raw_fd()) };
        if read_raw < 0 {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            return Err(CoreError::Io(io::Error::last_os_error()));
        }
        let read_fd = unsafe { OwnedFd::from_raw_fd(read_raw) };

        let (out_tx, out_rx) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("vdk-pty-read".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    let n = unsafe { libc::read(read_fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                    if n <= 0 {
                        break;
                    }
                    if out_tx.send(Bytes::copy_from_slice(&buf[..n as usize])).is_err() {
                        break;
                    }
                }
            })
            .map_err(CoreError::Io)?;

        let child_pid = Arc::new(Mutex::new(Some(pid)));
        let child_wait = child_pid.clone();
        let (exit_tx, exit_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("vdk-pty-wait".into())
            .spawn(move || {
                let mut status: libc::c_int = 0;
                let r = unsafe { libc::waitpid(pid, &raw mut status, 0) };
                child_wait.lock().unwrap_or_else(|e| e.into_inner()).take();
                let code = if r > 0 && libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status)
                } else if r > 0 && libc::WIFSIGNALED(status) {
                    128 + libc::WTERMSIG(status)
                } else {
                    -1
                };
                let _ = exit_tx.send(code);
            })
            .map_err(CoreError::Io)?;

        Ok(PtyStreams {
            pty: PtyProcess { master: Arc::new(Mutex::new(Some(master))), child_pid },
            output: out_rx,
            exit: exit_rx,
        })
    }

    pub fn write(&self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let guard = self.master.lock().unwrap_or_else(|e| e.into_inner());
        let Some(fd) = guard.as_ref() else {
            return Err(CoreError::Closed);
        };
        let mut offset = 0usize;
        while offset < data.len() {
            let n = unsafe {
                libc::write(fd.as_raw_fd(), data[offset..].as_ptr().cast(), data.len() - offset)
            };
            if n <= 0 {
                return Err(CoreError::Closed);
            }
            offset += n as usize;
        }
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let ws = libc::winsize {
            ws_row: rows.clamp(2, 1000),
            ws_col: cols.clamp(2, 1000),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let guard = self.master.lock().unwrap_or_else(|e| e.into_inner());
        let Some(fd) = guard.as_ref() else {
            return Ok(());
        };
        if unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &raw const ws) } < 0 {
            Err(CoreError::Io(io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        if let Some(pid) = *self.child_pid.lock().unwrap_or_else(|e| e.into_inner()) {
            unsafe {
                libc::kill(pid, libc::SIGHUP);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        self.master.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

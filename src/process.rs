use crate::Result;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

pub struct Managed(pub Child);
impl Managed {
    pub fn spawn(program: &str, args: &[&str], input: bool, output: bool) -> Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_remove("WAYLAND_DEBUG")
            .env_remove("ADB_TRACE")
            .stdin(if input { Stdio::piped() } else { Stdio::null() })
            .stdout(if output {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        // The child dies if the backend is killed, including during startup.
        // SAFETY: only async-signal-safe libc calls are made after fork.
        let parent = std::process::id() as libc::pid_t;
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        command
            .spawn()
            .map(Self)
            .map_err(|_| "Required helper unavailable; check ADB/Avahi/qrencode dependencies")
    }
    pub async fn stop(&mut self) {
        if let Some(pid) = self.0.id() {
            // SAFETY: pid belongs to our child's dedicated process group.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(1), self.0.wait())
            .await
            .is_err()
        {
            if let Some(pid) = self.0.id() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = self.0.wait().await;
        }
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        if let Some(pid) = self.0.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        // Tokio reaps kill_on_drop children; ordinary paths explicitly wait.
    }
}
pub async fn run(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
    limit: usize,
    seconds: u64,
) -> Result<Vec<u8>> {
    let mut child = Managed::spawn(program, args, input.is_some(), true)?;
    let result = tokio::time::timeout(Duration::from_secs(seconds), async {
        let mut stdout = child.0.stdout.take().unwrap();
        let stdin = child.0.stdin.take();
        let feed = async {
            if let Some(mut stdin) = stdin {
                stdin
                    .write_all(input.unwrap_or_default())
                    .await
                    .map_err(|_| "Helper input failed")?;
                stdin.shutdown().await.map_err(|_| "Helper input failed")?;
            }
            Ok(())
        };
        let read = async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| "Helper output failed")?;
            if bytes.len() > limit {
                return Err("Helper output exceeded its size limit");
            }
            Ok(bytes)
        };
        let ((), bytes) = tokio::try_join!(feed, read)?;
        if !child
            .0
            .wait()
            .await
            .map_err(|_| "Cannot reap helper")?
            .success()
        {
            return Err("Helper failed; check phone authorization and dependencies");
        }
        Ok(bytes)
    })
    .await
    .unwrap_or(Err("Command timed out; unlock or reconnect the phone"));
    child.stop().await;
    result
}

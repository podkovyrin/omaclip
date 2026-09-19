use crate::{
    Result,
    adb::{Adb, Device},
    process::Managed,
    protocol::VERSION,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{io::AsyncReadExt, net::TcpStream};
// Omarchy targets Arch Linux, whose scrcpy package owns this server.
// Never fall back to a downloaded or plugin-local copy.
pub fn server_path() -> PathBuf {
    PathBuf::from("/usr/share/scrcpy/scrcpy-server")
}
fn compatible_version(output: &[u8]) -> bool {
    let text = String::from_utf8_lossy(output);
    let mut words = text.split_whitespace();
    words.next() == Some("scrcpy") && words.next() == Some(VERSION)
}
pub async fn verify(path: &Path) -> Result<()> {
    let output = crate::process::run("/usr/bin/scrcpy", &["--version"], None, 8192, 5)
        .await
        .map_err(|_| "scrcpy is missing or cannot run; install scrcpy 4.1")?;
    if !compatible_version(&output) {
        return Err("This OmaClip release requires scrcpy 4.1; installed version is unsupported");
    }
    verify_archive(path)
}
fn verify_archive(path: &Path) -> Result<()> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|_| "System scrcpy server missing; reinstall the scrcpy package")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect system scrcpy server")?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Err("Invalid system scrcpy server; reinstall the scrcpy package");
    }
    let mut signature = [0; 4];
    file.read_exact(&mut signature)
        .map_err(|_| "Cannot read system scrcpy server; reinstall the scrcpy package")?;
    if signature != *b"PK\x03\x04" {
        return Err("Invalid system scrcpy server; reinstall the scrcpy package");
    }
    Ok(())
}

pub struct Transport {
    pub adb: Adb,
    pub device: Device,
    pub scid: String,
    remote: String,
    port: Option<u16>,
    pushed: bool,
    pub child: Option<Managed>,
}
impl Transport {
    pub fn new(adb: Adb, device: Device) -> Result<Self> {
        let mut scid = crate::random_hex(4)?;
        // scrcpy scid is a positive signed 31-bit integer.
        scid.replace_range(
            ..1,
            &format!("{:x}", u8::from_str_radix(&scid[..1], 16).unwrap() & 7),
        );
        Ok(Self {
            adb,
            device,
            remote: format!("/data/local/tmp/omaclip-{scid}.jar"),
            scid,
            port: None,
            pushed: false,
            child: None,
        })
    }
    pub async fn open(&mut self, identity: &str) -> Result<TcpStream> {
        let path = server_path();
        verify(&path).await?;
        if self.adb.identity(&self.device.id).await? != identity {
            return Err(
                "Selected endpoint identifies a different phone; select the intended phone again",
            );
        }
        self.check().await?;
        // Mark before push: cancellation or failed partial push still removes it.
        self.pushed = true;
        self.adb
            .call(
                &[
                    "-t",
                    &self.device.id,
                    "push",
                    path.to_str().ok_or("Invalid server path")?,
                    &self.remote,
                ],
                None,
                20,
            )
            .await?;
        let destination = format!("localabstract:scrcpy_{}", self.scid);
        // Fixed port chosen from a bound ephemeral listener before forwarding.
        // Remember it before await so cancellation can remove our own mapping.
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|_| "Cannot allocate ADB forward port")?;
        let port = listener
            .local_addr()
            .map_err(|_| "Cannot allocate ADB forward port")?
            .port();
        drop(listener);
        self.port = Some(port);
        self.adb
            .device(
                &self.device.id,
                &[
                    "forward",
                    "--no-rebind",
                    &format!("tcp:{port}"),
                    &destination,
                ],
            )
            .await?;
        let classpath = format!("CLASSPATH={}", self.remote);
        let scid = format!("scid={}", self.scid);
        self.child = Some(Managed::spawn(
            &self.adb.path,
            &[
                "-t",
                &self.device.id,
                "shell",
                &classpath,
                "app_process",
                "/",
                "com.genymobile.scrcpy.Server",
                VERSION,
                &scid,
                "log_level=error",
                "video=false",
                "audio=false",
                "control=true",
                "tunnel_forward=true",
                "send_dummy_byte=true",
                "send_device_meta=false",
                "clipboard_autosync=true",
                "power_on=false",
                "cleanup=true",
            ],
            false,
            false,
        )?);
        // Bounded retries only after connection/handshake failure during startup.
        for _ in 0..40 {
            let attempt = tokio::time::timeout(Duration::from_millis(500), async {
                let mut socket = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
                socket.set_nodelay(true).ok()?;
                if socket.read_u8().await.ok()? != 0 {
                    return None;
                }
                Some(socket)
            });
            tokio::select! {
                _=self.child.as_mut().unwrap().0.wait()=>break,
                socket=attempt=>if let Ok(Some(socket))=socket {return Ok(socket);}
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(
            "Android clipboard server did not start; unlock phone and check scrcpy 4.1 compatibility",
        )
    }
    pub async fn check(&self) -> Result<()> {
        self.adb.competitors(&self.device.id, &self.scid).await
    }
    pub async fn close(&mut self) {
        if let Some(child) = &mut self.child {
            child.stop().await;
        }
        self.child = None;
        if let Some(port) = self.port.take() {
            // A cancelled creation may have installed the mapping before its
            // response arrived. Verify ownership; --no-rebind failure must not
            // remove a mapping belonging to another tool.
            if let Ok(list) = self.adb.call(&["forward", "--list"], None, 3).await {
                let local = format!("tcp:{port}");
                let remote = format!("localabstract:scrcpy_{}", self.scid);
                let owns = String::from_utf8_lossy(&list).lines().any(|line| {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    fields.len() == 3 && fields[1] == local && fields[2] == remote
                });
                if owns {
                    let _ = self
                        .adb
                        .device(&self.device.id, &["forward", "--remove", &local])
                        .await;
                }
            }
        }
        if self.pushed {
            // Exact random session token; never terminate someone else's server.
            // app_process may outlive a killed adb client while awaiting accept.
            let script = format!(
                "scid={}; needle=\"com.genymobile.scrcpy.Server 4.1 scid=$scid\"; ps -A -o PID,ARGS | while read -r pid args; do case \" $args \" in *\" $needle \"*) kill \"$pid\";; esac; done; rm -f {}",
                self.scid, self.remote
            );
            let _ = self.adb.device(&self.device.id, &["shell", &script]).await;
            self.pushed = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_supported_scrcpy_version() {
        assert!(compatible_version(
            b"scrcpy 4.1 <https://github.com/Genymobile/scrcpy>\nDependencies:"
        ));
        for output in [
            b"scrcpy 4.10".as_slice(),
            b"scrcpy 4.1-dev",
            b"scrcpy 4.0",
            b"scrcpy 5.0",
            b"",
            b"something 4.1",
        ] {
            assert!(!compatible_version(output));
        }
    }

    #[test]
    fn rejects_missing_or_invalid_server() {
        let path = std::env::temp_dir().join(format!(
            "omaclip-server-test-{}",
            crate::random_hex(8).unwrap()
        ));
        assert!(verify_archive(&path).is_err());
        std::fs::write(&path, b"not a server").unwrap();
        assert!(verify_archive(&path).is_err());
        std::fs::write(&path, b"PK").unwrap();
        assert!(verify_archive(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
}

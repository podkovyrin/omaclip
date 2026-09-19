use crate::{Cancel, Result, process};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
};

#[derive(Clone)]
pub struct Adb {
    pub path: String,
    pub socket: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub model: String,
    pub transport: String,
    #[serde(skip)]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<String>,
}
impl Device {
    pub fn key(&self) -> (&str, &str) {
        (&self.serial, &self.id)
    }
}
pub fn parse_devices(raw: &str) -> Vec<Device> {
    raw.lines()
        .filter_map(|line| {
            let p: Vec<_> = line.split_whitespace().collect();
            if p.len() < 2 || p[0].len() > 512 {
                return None;
            }
            let state = if p[1] == "no" && p.get(2) == Some(&"permissions") {
                "no permissions"
            } else {
                p[1]
            };
            if ![
                "device",
                "offline",
                "unauthorized",
                "authorizing",
                "no permissions",
            ]
            .contains(&state)
            {
                return None;
            }
            let meta = |key: &str| p.iter().find_map(|v| v.strip_prefix(key));
            Some(Device {
                serial: p[0].into(),
                state: state.into(),
                model: meta("model:").unwrap_or(p[0]).replace('_', " "),
                transport: if p[0].contains(':') || p[0].contains("_adb-tls-connect") {
                    "Wi-Fi"
                } else {
                    "USB"
                }
                .into(),
                id: meta("transport_id:").unwrap_or_default().into(),
                identity: None,
                connections: vec![],
            })
        })
        .take(64)
        .collect()
}
pub fn group(devices: &[Device], selected: &str) -> Vec<Device> {
    let mut groups: BTreeMap<(bool, &str), Vec<&Device>> = BTreeMap::new();
    for d in devices {
        let identity = d.identity.as_deref().filter(|_| d.state == "device");
        groups
            .entry((identity.is_some(), identity.unwrap_or(&d.serial)))
            .or_default()
            .push(d);
    }
    groups
        .into_values()
        .map(|mut ds| {
            ds.sort_by_key(|d| {
                (
                    d.serial != selected,
                    d.transport != "USB",
                    !d.serial.contains("._adb-tls-connect"),
                    &d.serial,
                )
            });
            let mut row = ds[0].clone();
            row.connections = ds.iter().map(|d| d.serial.clone()).collect();
            row.connections.sort();
            let mut transports: Vec<_> = ds.iter().map(|d| d.transport.as_str()).collect();
            transports.sort();
            transports.dedup();
            row.transport = transports.join(" / ");
            row
        })
        .collect()
}
pub fn endpoint(value: &str) -> Result<&str> {
    let value = value.trim();
    let (host, port) = value
        .rsplit_once(':')
        .ok_or("Use host:port or [IPv6]:port; pairing and connection ports differ")?;
    let good_host = if host.starts_with('[') && host.ends_with(']') {
        host[1..host.len() - 1]
            .parse::<std::net::Ipv6Addr>()
            .is_ok()
    } else {
        !host.is_empty()
            && host.len() <= 253
            && host.as_bytes()[0].is_ascii_alphanumeric()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    };
    if !good_host || port.parse::<u16>().ok().filter(|p| *p != 0).is_none() {
        return Err("Use a valid host and explicit port; pairing and connection ports differ");
    }
    Ok(value)
}
impl Adb {
    pub fn new(path: String) -> Result<Self> {
        let path = if path.is_empty() {
            if std::env::var_os("PATH")
                .is_some_and(|p| std::env::split_paths(&p).any(|p| p.join("adb").is_file()))
            {
                "adb".into()
            } else {
                format!(
                    "{}/Android/Sdk/platform-tools/adb",
                    std::env::var("HOME").unwrap_or_default()
                )
            }
        } else {
            path
        };
        let socket = std::env::var("ADB_SERVER_SOCKET").unwrap_or_else(|_| {
            format!(
                "tcp:localhost:{}",
                std::env::var("ANDROID_ADB_SERVER_PORT").unwrap_or("5037".into())
            )
        });
        let address = socket
            .strip_prefix("tcp:")
            .ok_or("ADB_SERVER_SOCKET must be a local TCP smart socket")?;
        let socket = if address.contains(':') {
            address.to_owned()
        } else {
            format!("localhost:{address}")
        };
        endpoint(&socket)?;
        let host = socket.rsplit_once(':').unwrap().0.trim_matches(['[', ']']);
        if host != "localhost"
            && !host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        {
            return Err("Remote ADB servers are unsupported; use a loopback ADB_SERVER_SOCKET");
        }
        Ok(Self { path, socket })
    }
    pub async fn call(&self, args: &[&str], input: Option<&[u8]>, seconds: u64) -> Result<Vec<u8>> {
        process::run(&self.path, args, input, 65536, seconds).await
    }
    pub async fn device(&self, id: &str, args: &[&str]) -> Result<Vec<u8>> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err("ADB omitted transport IDs; update platform-tools");
        }
        let mut all = vec!["-t", id];
        all.extend_from_slice(args);
        self.call(&all, None, 3).await
    }
    pub async fn identity(&self, id: &str) -> Result<String> {
        let b = self
            .device(id, &["shell", "getprop", "ro.serialno"])
            .await?;
        let value = std::str::from_utf8(&b)
            .map_err(|_| "Invalid phone identity")?
            .trim();
        if value.is_empty() || value.eq_ignore_ascii_case("unknown") || value.len() > 256 {
            return Err("Phone exposes no stable ro.serialno; cannot safely reconnect");
        }
        Ok(value.into())
    }
    pub async fn competitors(&self, id: &str, own: &str) -> Result<()> {
        if local_scrcpy(Path::new("/proc")) {
            return Err("Paused for scrcpy; close scrcpy/Android Mirror to resume");
        }
        let raw = self
            .device(id, &["shell", "ps", "-A", "-o", "ARGS"])
            .await?;
        if other_server(&String::from_utf8_lossy(&raw), own) {
            return Err("Paused for another Android scrcpy server; close its client to resume");
        }
        Ok(())
    }
    pub async fn connect(&self, address: &str) -> Result<()> {
        let raw = self
            .call(&["connect", endpoint(address)?], None, 15)
            .await?;
        if !raw
            .split(|b| *b == b'\n')
            .any(|l| l.starts_with(b"connected to ") || l.starts_with(b"already connected to "))
        {
            return Err(
                "Connection failed; use the main Wireless debugging connection port, not the pairing port",
            );
        }
        Ok(())
    }
    pub async fn pair(&self, address: &str, secret: &str, qr: bool) -> Result<String> {
        let valid = if qr {
            (20..=64).contains(&secret.len()) && secret.bytes().all(|c| c.is_ascii_alphanumeric())
        } else {
            secret.len() == 6 && secret.bytes().all(|c| c.is_ascii_digit())
        };
        if !valid {
            return Err("Invalid pairing code; generate a new code on the phone");
        }
        let input = format!("{secret}\n");
        let raw = self
            .call(&["pair", endpoint(address)?], Some(input.as_bytes()), 30)
            .await?;
        let s = String::from_utf8_lossy(&raw);
        if !s.contains("Successfully paired") {
            return Err("Pairing failed; generate a new QR/code and check the pairing port");
        }
        Ok(s.split_once("[guid=")
            .and_then(|(_, s)| s.split_once(']'))
            .map(|(s, _)| s)
            .filter(|s| {
                s.len() <= 256
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
            })
            .unwrap_or_default()
            .into())
    }
}
pub fn local_scrcpy(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return true;
    };
    use std::os::unix::fs::MetadataExt;
    entries.flatten().any(|e| {
        if !e
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|c| c.is_ascii_digit())
        {
            return false;
        }
        if !e
            .metadata()
            .is_ok_and(|m| m.uid() == unsafe { libc::getuid() })
        {
            return false;
        }
        std::fs::read(e.path().join("cmdline")).is_ok_and(|b| {
            let first = b.split(|b| *b == 0).next().unwrap_or_default();
            matches!(
                first.rsplit(|b| *b == b'/').next(),
                Some(b"scrcpy" | b"scrcpy.exe")
            )
        })
    })
}
pub fn other_server(raw: &str, own: &str) -> bool {
    raw.lines().any(|line| {
        line.contains("com.genymobile.scrcpy.Server")
            && (own.is_empty()
                || !line
                    .split_whitespace()
                    .any(|v| v.strip_prefix("scid=") == Some(own)))
    })
}
pub async fn track_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Result<Vec<Device>> {
    let mut header = [0; 4];
    r.read_exact(&mut header)
        .await
        .map_err(|_| "ADB tracking disconnected; restarting discovery")?;
    let n = std::str::from_utf8(&header)
        .ok()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or("Invalid ADB tracking frame")?;
    let mut data = vec![0; n];
    r.read_exact(&mut data)
        .await
        .map_err(|_| "Truncated ADB tracking frame")?;
    let s = std::str::from_utf8(&data).map_err(|_| "Invalid ADB device list")?;
    Ok(parse_devices(s))
}
#[derive(Default)]
pub struct Identities(HashMap<(String, String), Option<String>>);
impl Identities {
    pub fn reconcile(&mut self, ds: &mut [Device]) {
        self.0.retain(|(serial, id), _| {
            ds.iter()
                .any(|d| &d.serial == serial && &d.id == id && d.state == "device")
        });
        for d in ds {
            d.identity = self
                .0
                .get(&(d.serial.clone(), d.id.clone()))
                .cloned()
                .flatten();
        }
    }
    pub fn known(&self, d: &Device) -> bool {
        self.0.contains_key(&(d.serial.clone(), d.id.clone()))
    }
    pub fn put(&mut self, d: &mut Device, value: Option<String>) {
        self.0
            .insert((d.serial.clone(), d.id.clone()), value.clone());
        d.identity = value;
    }
}
pub async fn track(
    adb: Adb,
    tx: watch::Sender<Vec<Device>>,
    error: watch::Sender<Option<&'static str>>,
    cancel: Cancel,
) {
    let work = async {
        let mut delay = 1;
        loop {
            let result: Result<()> = async {
                adb.call(&["start-server"], None, 10).await?;
                let mut socket =
                    tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&adb.socket))
                        .await
                        .map_err(|_| "ADB tracking connect timed out")?
                        .map_err(|_| "Cannot connect to ADB server")?;
                let request = b"host:track-devices-l";
                socket
                    .write_all(format!("{:04x}", request.len()).as_bytes())
                    .await
                    .map_err(|_| "ADB tracking failed")?;
                socket
                    .write_all(request)
                    .await
                    .map_err(|_| "ADB tracking failed")?;
                let mut status = [0; 4];
                tokio::time::timeout(Duration::from_secs(5), socket.read_exact(&mut status))
                    .await.map_err(|_| "ADB tracking handshake timed out")?
                    .map_err(|_| "ADB tracking failed")?;
                if &status != b"OKAY" {
                    return Err("ADB device tracking unsupported; update platform-tools");
                }
                let (frames, mut incoming) = tokio::sync::mpsc::channel(16);
                let reader = tokio::spawn(async move {
                    loop {
                        let frame = track_frame(&mut socket).await;
                        let failed = frame.is_err();
                        if frames.send(frame).await.is_err() || failed { break; }
                    }
                });
                let _reader = TrackingReader(reader);
                let mut cache = Identities::default();
                let mut ds: Vec<Device> = Vec::new();
                let mut probes = tokio::task::JoinSet::new();
                let mut in_flight = std::collections::HashSet::new();
                let mut generation = 0u64;
                loop {
                    for d in &ds {
                        if probes.len() >= 4 { break; }
                        if d.state == "device" && !cache.known(d) && in_flight.insert((d.serial.clone(), d.id.clone())) {
                            let adb = adb.clone(); let d = d.clone();
                            probes.spawn(async move { let identity = adb.identity(&d.id).await.ok(); (generation, d, identity) });
                        }
                    }
                    tx.send_if_modified(|old| {
                        if *old != ds { *old = ds.clone(); true } else { false }
                    });
                    tokio::select! {
                        biased;
                        frame = incoming.recv() => {
                            ds = frame.ok_or("ADB tracking disconnected; restarting discovery")??;
                            // Device events interrupt probes immediately. Late results from
                            // a disappeared/offline transport cannot populate a new epoch.
                            generation += 1;
                            probes.abort_all(); in_flight.clear();
                            cache.reconcile(&mut ds);
                            error.send_if_modified(|old| old.take().is_some()); delay = 1;
                        }
                        result = probes.join_next(), if !probes.is_empty() => {
                            if let Some(Ok((g, old, identity))) = result && g == generation {
                                in_flight.remove(&(old.serial.clone(), old.id.clone()));
                                if let Some(d) = ds.iter_mut().find(|d| d.key() == old.key() && d.state == "device") {
                                    cache.put(d, identity);
                                }
                            }
                        }
                    }
                }
            }
            .await;
            tx.send_replace(vec![]);
            error.send_replace(result.err());
            tokio::time::sleep(Duration::from_secs(delay)).await;
            delay = (delay * 2).min(15);
        }
    };
    tokio::select! {_ = cancel.cancelled()=>{}, _=work=>{}}
}

struct TrackingReader(tokio::task::JoinHandle<()>);
impl Drop for TrackingReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

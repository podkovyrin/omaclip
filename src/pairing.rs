use crate::{
    Cancel, Result,
    adb::{Adb, Device, endpoint},
    process::{self, Managed},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::watch,
    time::Instant,
};
#[derive(Clone, Default, PartialEq)]
pub struct Status {
    pub image: String,
    pub message: String,
    pub active: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub kind: String,
    pub address: String,
    pub target: String,
}
fn unescape(raw: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut bytes = raw.bytes();
    while let Some(b) = bytes.next() {
        if b == b'\\' {
            let a = bytes.next()?;
            let b = bytes.next()?;
            let c = bytes.next()?;
            if ![a, b, c].iter().all(u8::is_ascii_digit) {
                return None;
            }
            let n = (a - b'0') as u16 * 100 + (b - b'0') as u16 * 10 + (c - b'0') as u16;
            out.push(u8::try_from(n).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}
pub fn parse_service(line: &str) -> Option<(bool, Service)> {
    let p: Vec<_> = line.trim_end().split(';').collect();
    if p.len() < 6 || !["=", "-"].contains(&p[0]) {
        return None;
    }
    let name = unescape(p[3])?;
    let kind = p[4];
    if !["_adb-tls-pairing._tcp", "_adb-tls-connect._tcp"].contains(&kind) || p[5] != "local" {
        return None;
    }
    let address = if p[0] == "=" {
        if p.len() < 9 {
            return None;
        }
        let host = p[7];
        let port = p[8];
        let value = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        endpoint(&value).ok()?;
        value
    } else {
        String::new()
    };
    Some((
        p[0] == "=",
        Service {
            name,
            kind: kind.into(),
            address,
            target: p.get(6).unwrap_or(&"").to_string(),
        },
    ))
}
pub fn connected(devices: &[Device], guid: &str, services: &[Service]) -> bool {
    !guid.is_empty()
        && devices.iter().any(|d| {
            d.state == "device"
                && (matches_guid(
                    d.serial
                        .split("._adb-tls-connect")
                        .next()
                        .unwrap_or_default(),
                    guid,
                ) || services.iter().any(|s| {
                    s.kind == "_adb-tls-connect._tcp"
                        && matches_guid(&s.name, guid)
                        && s.address == d.serial
                }))
        })
}
pub fn matches_guid(name: &str, guid: &str) -> bool {
    !guid.is_empty() && (name == guid || name.strip_prefix("adb-") == Some(guid))
}
async fn browse(kind: &str, tx: watch::Sender<Vec<Service>>, cancel: Cancel) -> Result<()> {
    let mut child = Managed::spawn(
        "avahi-browse",
        &["--parsable", "--resolve", "--no-db-lookup", kind],
        false,
        true,
    )?;
    let mut reader = BufReader::new(child.0.stdout.take().unwrap());
    let result = tokio::select! {
        _=cancel.cancelled()=>Ok(()),
        result=async {
            let mut rows=BTreeMap::new();
            loop {
                let mut bytes=Vec::new();
                use tokio::io::AsyncReadExt;
                let n=(&mut reader).take(8193).read_until(b'\n',&mut bytes).await.map_err(|_|"mDNS discovery stopped; check avahi-daemon")?;
                if n==0 {return Err("mDNS discovery stopped; install avahi and start avahi-daemon");}
                if n>8192 {return Err("Oversized mDNS discovery event");}
                if let Ok(line)=std::str::from_utf8(&bytes) && let Some((add,service))=parse_service(line) {
                    // IPv4/IPv6/interface records are independent; removing one
                    // must not remove a still-live address on another interface.
                    let p:Vec<_>=line.trim_end().split(';').collect();
                    let key=(p[1].to_owned(),p[2].to_owned(),service.name.clone());
                    if add {if rows.len()<128 || rows.contains_key(&key) {rows.insert(key,service);}} else {rows.remove(&key);}
                    tx.send_replace(rows.values().cloned().collect());
                }
            }
        }=>result,
    };
    child.stop().await;
    result
}
pub async fn qr_image(payload: &str) -> Result<String> {
    let png = process::run(
        "qrencode",
        &["-t", "PNG", "-s", "6", "-m", "4", "-o", "-"],
        Some(payload.as_bytes()),
        128 * 1024,
        5,
    )
    .await?;
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Cannot generate QR; install qrencode");
    }
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(png)))
}
pub async fn run(
    adb: Adb,
    mut devices: watch::Receiver<Vec<Device>>,
    status: watch::Sender<Status>,
    cancel: Cancel,
) -> Result<()> {
    let browse_cancel = Cancel::new();
    let (ptx, mut pairing) = watch::channel(vec![]);
    let (ctx, mut connection) = watch::channel(vec![]);
    let pc = browse_cancel.clone();
    let cc = browse_cancel.clone();
    let mut p = tokio::spawn(async move { browse("_adb-tls-pairing._tcp", ptx, pc).await });
    let mut c = tokio::spawn(async move { browse("_adb-tls-connect._tcp", ctx, cc).await });
    let work = async {
        let name = format!("studio-{}", crate::random_hex(8)?);
        let secret = crate::random_hex(16)?;
        let image = qr_image(&format!("WIFI:T:ADB;S:{name};P:{secret};;")).await?;
        status.send_replace(Status {
            image,
            message:
                "On Android: Wireless debugging → Pair device with QR code. Scan within 2 minutes"
                    .into(),
            active: true,
        });
        let guid = loop {
            let services = pairing.borrow_and_update().clone();
            let matches: Vec<_> = services.iter().filter(|s| s.name == name).collect();
            if matches
                .iter()
                .map(|s| &s.target)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > 1
            {
                return Err("Multiple phones scanned this QR; cancel and pair one phone at a time");
            }
            if let Some(service) = matches.first() {
                // Prefer one IPv4 endpoint for a multi-address advertisement.
                // The random instance+secret authenticates this exact QR attempt.
                status.send_replace(Status {
                    message: "Phone found. Pairing…".into(),
                    active: true,
                    ..Status::default()
                });
                break adb.pair(&service.address, &secret, true).await?;
            }
            pairing
                .changed()
                .await
                .map_err(|_| "mDNS pairing discovery stopped")?;
        };
        drop(secret);
        status.send_replace(Status {
            message: "Paired. Waiting for ADB's automatic Wi-Fi connection…".into(),
            active: true,
            ..Status::default()
        });
        let auto_deadline = Instant::now() + Duration::from_secs(3);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut fallback = false;
        loop {
            let services = connection.borrow_and_update().clone();
            if connected(&devices.borrow_and_update(), &guid, &services) {
                return Ok("Paired and connected. Select your phone, then enable sync");
            }
            if Instant::now() >= auto_deadline
                && !fallback
                && let Some(service) = services.iter().find(|s| matches_guid(&s.name, &guid))
            {
                // Final current tracker check immediately before explicit connect.
                if !connected(&devices.borrow(), &guid, &services) {
                    adb.connect(&service.address).await?;
                }
                fallback = true;
            }
            tokio::select! {
                r=devices.changed()=>{r.map_err(|_|"ADB tracking stopped")?;},
                r=connection.changed()=>{r.map_err(|_|"mDNS connection discovery stopped")?;},
                _=tokio::time::sleep_until(auto_deadline),if Instant::now()<auto_deadline=>{},
                _=tokio::time::sleep_until(deadline)=>return Ok("Paired. If absent, use the main Wireless debugging connection address below"),
            }
        }
    };
    let result = tokio::select! {
        _=cancel.cancelled()=>Ok("QR pairing closed"),
        _=tokio::time::sleep(Duration::from_secs(120))=>Ok("QR expired. Generate a new code; allow mDNS on the same Wi-Fi network"),
        result=&mut p=>Err(result.ok().and_then(Result::err).unwrap_or("mDNS pairing discovery stopped")),
        result=&mut c=>Err(result.ok().and_then(Result::err).unwrap_or("mDNS connection discovery stopped")),
        result=work=>result,
    };
    browse_cancel.cancel();
    if !p.is_finished() {
        let _ = p.await;
    }
    if !c.is_finished() {
        let _ = c.await;
    }
    status.send_replace(Status {
        message: result.unwrap_or_else(|e| e).into(),
        ..Status::default()
    });
    result.map(|_| ())
}

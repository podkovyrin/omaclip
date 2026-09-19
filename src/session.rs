use crate::{
    Cancel, Result,
    adb::{Adb, Device},
    protocol::{self, Message},
    sync::Policy,
    transport::Transport,
    wayland::Clipboard,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
// A single owner serializes decisions. Framing lives in a dedicated reader so
// select! never cancels a partial read. Queue and ACK window are bounded.
pub async fn run(
    adb: Adb,
    device: Device,
    identity: String,
    clipboard: Clipboard,
    ready: watch::Sender<bool>,
    cancel: Cancel,
) -> Result<()> {
    let mut transport = Transport::new(adb, device)?;
    let result = tokio::select! {
        biased;
        _=cancel.cancelled()=>Ok(()),
        result=active(&mut transport,&identity,clipboard,ready)=>result,
    };
    transport.close().await;
    result
}
async fn active(
    transport: &mut Transport,
    identity: &str,
    clipboard: Clipboard,
    ready: watch::Sender<bool>,
) -> Result<()> {
    let socket = transport.open(identity).await?;
    let (mut reader, mut writer) = socket.into_split();
    let (tx, mut messages) = mpsc::channel(8);
    let read_task = tokio::spawn(async move {
        loop {
            let value = protocol::read(&mut reader).await;
            let failed = value.is_err();
            if tx.send(value).await.is_err() || failed {
                break;
            }
        }
    });
    // Abort-on-drop guard is needed when the entire epoch is cancelled.
    let guard = ReadGuard(read_task);
    let mut desktop = clipboard.events.clone();
    let baseline = clipboard.baseline().await?;
    if desktop.borrow().revision == baseline.revision {
        desktop.borrow_and_update();
    }
    // Changes arriving during startup are baselines, never replayed.
    while let Ok(message) = messages.try_recv() {
        message?;
    }
    let mut policy = Policy::new(baseline.bytes());
    let mut revision = baseline.revision;
    drop(baseline);
    let mut sequence = 0u64;
    let mut pending = std::collections::BTreeMap::new();
    ready.send_replace(true);
    let result=async {
        loop {
            let deadline=pending.values().next().copied();
            tokio::select! {
                biased;
                _=transport.child.as_mut().unwrap().0.wait()=>return Err("Android server exited; reconnecting"),
                _=async {match deadline {Some(d)=>tokio::time::sleep_until(d).await,None=>std::future::pending().await}}=>return Err("Phone stopped acknowledging updates; unlock or reconnect it"),
                changed=desktop.changed()=>{
                    changed.map_err(|_|"Wayland clipboard closed; restart bridge")?;
                    let snapshot=desktop.borrow_and_update().clone();
                    revision=snapshot.revision;
                    if !snapshot.ready {continue;}
                    if snapshot.own {if let Some(data)=snapshot.bytes() {policy.applied(data);}continue;}
                    if let Some(data) = snapshot.bytes() {
                        transport.check().await?;
                        // If a newer selection arrived during checks, process it instead.
                        if desktop.borrow().revision!=revision {continue;}
                        if !policy.desktop(Some(data)) {continue;}
                        if pending.len()>=64 {return Err("Phone is not acknowledging clipboard updates; reconnect it");}
                        sequence=sequence.checked_add(1).ok_or("Clipboard sequence exhausted; reconnect")?;
                        tokio::time::timeout(Duration::from_secs(3),protocol::write(&mut writer,data,sequence)).await.map_err(|_|"Clipboard write timed out")??;
                        policy.sent(data);pending.insert(sequence,Instant::now()+Duration::from_secs(5));
                    } else { policy.desktop(None); }
                }
                message=messages.recv()=>match message.ok_or("Android control channel closed")?? {
                    Message::Ack(seq)=>if pending.remove(&seq).is_none() {return Err("Unexpected clipboard acknowledgement; reconnect the server");},
                    Message::Text(Some(data)) if policy.phone(&data)=>{
                        transport.check().await?;
                        // Native Wayland adds a display.sync barrier and verifies the
                        // same selection generation before taking ownership.
                        let data:Arc<[u8]>=Arc::from(data);
                        if clipboard.write(revision,data.clone()).await? {policy.applied(&data);}
                    }
                    _=>{}
                }
            }
        }
    }.await;
    drop(writer);
    drop(guard);
    result
}
struct ReadGuard(tokio::task::JoinHandle<()>);
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

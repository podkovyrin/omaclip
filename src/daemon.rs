use crate::{
    Cancel, Result,
    adb::{self, Adb, Device},
    config::Config,
    pairing, session, wayland,
};
use serde::Serialize;
use serde_json::Value;
use std::{path::PathBuf, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};
struct Job {
    cancel: Cancel,
    handle: JoinHandle<Result<()>>,
}
impl Job {
    async fn stop(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}
#[derive(Serialize, PartialEq)]
struct Status {
    state: String,
    message: String,
    action: String,
    selected: String,
    enabled: bool,
    devices: Vec<Device>,
    #[serde(rename = "qrImage")]
    qr_image: String,
    #[serde(rename = "qrActive")]
    qr_active: bool,
}
pub struct Daemon {
    adb: Adb,
    config: Config,
    path: PathBuf,
    devices: watch::Receiver<Vec<Device>>,
    errors: watch::Receiver<Option<&'static str>>,
    clipboard: wayland::Clipboard,
    session: Option<Job>,
    session_key: Option<(String, String)>,
    ready: watch::Receiver<bool>,
    ready_tx: watch::Sender<bool>,
    qr: Option<Job>,
    qr_status: watch::Receiver<pairing::Status>,
    qr_tx: watch::Sender<pairing::Status>,
    action_job: Option<Job>,
    action_result: watch::Receiver<String>,
    action_tx: watch::Sender<String>,
    state: String,
    message: String,
    action: String,
    retry: Option<Instant>,
    delay: u64,
}
impl Daemon {
    fn new(
        adb: Adb,
        path: PathBuf,
        devices: watch::Receiver<Vec<Device>>,
        errors: watch::Receiver<Option<&'static str>>,
        clipboard: wayland::Clipboard,
    ) -> Self {
        let (ready_tx, ready) = watch::channel(false);
        let (qr_tx, qr_status) = watch::channel(pairing::Status::default());
        let (action_tx, action_result) = watch::channel(String::new());
        Self {
            adb,
            config: Config::load(&path),
            path,
            devices,
            errors,
            clipboard,
            session: None,
            session_key: None,
            ready,
            ready_tx,
            qr: None,
            qr_status,
            qr_tx,
            action_job: None,
            action_result,
            action_tx,
            state: "paused".into(),
            message: "Select one phone, then enable clipboard sync".into(),
            action: String::new(),
            retry: None,
            delay: 1,
        }
    }
    fn status(&self) -> Status {
        let qr = self.qr_status.borrow();
        Status {
            state: self.state.clone(),
            message: self.message.clone(),
            action: self.action.clone(),
            selected: self.config.serial.clone(),
            enabled: self.config.enabled,
            devices: adb::group(&self.devices.borrow(), &self.config.serial),
            qr_image: qr.image.clone(),
            qr_active: qr.active,
        }
    }
    async fn stop_session(&mut self) {
        self.session_key = None;
        self.retry = None;
        if let Some(job) = self.session.take() {
            job.stop().await;
        }
        self.ready_tx.send_replace(false);
        self.clipboard.suspend().await;
    }
    async fn stop_qr(&mut self) {
        if let Some(job) = self.qr.take() {
            job.stop().await;
        }
        self.qr_tx.send_replace(pairing::Status::default());
        self.qr_status.borrow_and_update();
    }
    async fn command(&mut self, value: Value) -> Result<()> {
        let op = value
            .get("op")
            .and_then(Value::as_str)
            .ok_or("Expected a JSON command object")?;
        match op {
            "select" | "enable" | "disconnect" => {
                self.stop_qr().await;
                self.config.enabled = false;
                self.stop_session().await;
                self.state = "paused".into();
                self.message = "Paused; no clipboard changes are being shared".into();
                self.config.save(&self.path)?;
                match op {
                    "select" => {
                        let serial = value
                            .get("serial")
                            .and_then(Value::as_str)
                            .ok_or("Select a ready phone from the list")?;
                        let device = self
                            .devices
                            .borrow()
                            .iter()
                            .find(|d| d.serial == serial && d.state == "device")
                            .cloned()
                            .ok_or("Select a ready, authorized phone from the list")?;
                        let identity = self.adb.identity(&device.id).await?;
                        self.config.serial = device.serial;
                        self.config.identity = identity;
                        self.message = "Phone selected. Enable sync to share future copies".into();
                    }
                    "enable" => {
                        let enabled = value
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .ok_or("enabled must be true or false")?;
                        if enabled
                            && (self.config.serial.is_empty() || self.config.identity.is_empty())
                        {
                            return Err("Select a phone before enabling clipboard sync");
                        }
                        self.config.enabled = enabled;
                        self.message = if enabled {
                            "Connecting; existing clipboards are preserved"
                        } else {
                            "Paused; copies made while paused will not be replayed"
                        }
                        .into();
                    }
                    _ => {
                        self.config.serial.clear();
                        self.config.identity.clear();
                        self.message = "Disconnected; no phone selected".into();
                    }
                }
                self.config.save(&self.path)?;
                self.delay = 1;
            }
            "refresh" => {
                self.retry = None;
            } // tracker is authoritative; no adb devices polling
            "qr_cancel" => {
                self.stop_qr().await;
                self.action = "QR pairing closed".into();
            }
            "qr_start" => {
                self.stop_qr().await;
                if self
                    .action_job
                    .as_ref()
                    .is_some_and(|j| !j.handle.is_finished())
                {
                    return Err("Wait for the current Wi-Fi action to finish");
                }
                let cancel = Cancel::new();
                let c = cancel.clone();
                let adb = self.adb.clone();
                let devices = self.devices.clone();
                let tx = self.qr_tx.clone();
                self.action = "Generating QR code…".into();
                self.qr = Some(Job {
                    cancel,
                    handle: tokio::spawn(async move { pairing::run(adb, devices, tx, c).await }),
                });
            }
            "pair" | "connect" => {
                self.stop_qr().await;
                if self
                    .action_job
                    .as_ref()
                    .is_some_and(|j| !j.handle.is_finished())
                {
                    return Err("Wait for the current Wi-Fi action to finish");
                }
                if let Some(job) = self.action_job.take() {
                    job.stop().await;
                }
                let address = value
                    .get("address")
                    .and_then(Value::as_str)
                    .ok_or("Enter the phone address and port")?
                    .to_owned();
                adb::endpoint(&address)?;
                let pair = op == "pair";
                let code = value
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let adb = self.adb.clone();
                let tx = self.action_tx.clone();
                let cancel = Cancel::new();
                let c = cancel.clone();
                self.action = if pair {
                    "Pairing…"
                } else {
                    "Connecting Wi-Fi…"
                }
                .into();
                self.action_job = Some(Job {
                    cancel,
                    handle: tokio::spawn(async move {
                        let result = tokio::select! {
                            _=c.cancelled()=>return Ok(()),
                            result=async {if pair {adb.pair(&address,&code,false).await.map(|_|())}else{adb.connect(&address).await}}=>result,
                        };
                        tx.send_replace(match result {Ok(())=>if pair {"Paired. Use the separate connection port from Wireless debugging"}else{"ADB connected. Select the intended phone from the list"},Err(e)=>e}.into());
                        result
                    }),
                });
            }
            _ => {
                return Err(
                    "Unknown command; use refresh, select, enable, disconnect, qr_start, qr_cancel, pair or connect",
                );
            }
        }
        Ok(())
    }
    async fn reconcile(&mut self) {
        if !self.config.enabled {
            return;
        }
        let device = {
            let devices = self.devices.borrow();
            let selected = devices
                .iter()
                .find(|d| d.serial == self.config.serial && d.state == "device");
            selected
                .filter(|d| {
                    d.identity
                        .as_ref()
                        .is_none_or(|id| id == &self.config.identity)
                })
                .or_else(|| {
                    devices
                        .iter()
                        .filter(|d| {
                            d.state == "device"
                                && !self.config.identity.is_empty()
                                && d.identity.as_ref() == Some(&self.config.identity)
                        })
                        .min_by_key(|d| {
                            (
                                d.transport != "USB",
                                !d.serial.contains("._adb-tls-connect"),
                                &d.serial,
                            )
                        })
                })
                // Keep the selected endpoint's identity error visible if it was reused.
                .or(selected)
                .cloned()
        };
        let key = device.as_ref().map(|d| (d.serial.clone(), d.id.clone()));
        if self.session.is_some() && key != self.session_key {
            self.stop_session().await;
        }
        let Some(device) = device else {
            self.state = "waiting".into();
            let ds = self.devices.borrow();
            let state = ds
                .iter()
                .find(|d| d.serial == self.config.serial)
                .map(|d| d.state.as_str());
            self.message = match state {
                Some("unauthorized" | "authorizing") => {
                    "Unlock phone and accept USB debugging authorization"
                }
                Some("offline") => "Phone offline; reconnect USB or Wi-Fi",
                Some("no permissions") => {
                    "USB permission denied; install android-udev and reconnect"
                }
                _ => self
                    .errors
                    .borrow()
                    .unwrap_or("Waiting for remembered phone over USB or paired Wi-Fi"),
            }
            .into();
            return;
        };
        if device.serial != self.config.serial {
            self.config.serial = device.serial.clone();
            if let Err(error) = self.config.save(&self.path) {
                self.action = error.into();
            }
            self.retry = None;
            self.delay = 1;
        }
        if self.session.is_none() && self.retry.is_none_or(|deadline| deadline <= Instant::now()) {
            self.retry = None;
            self.ready_tx.send_replace(false);
            self.ready.borrow_and_update();
            self.state = "connecting".into();
            self.message = "Starting clipboard-only server; preserving existing clipboards".into();
            let cancel = Cancel::new();
            let c = cancel.clone();
            let adb = self.adb.clone();
            let identity = self.config.identity.clone();
            let clipboard = self.clipboard.clone();
            let ready = self.ready_tx.clone();
            self.session_key = key;
            self.session = Some(Job {
                cancel,
                handle: tokio::spawn(async move {
                    session::run(adb, device, identity, clipboard, ready, c).await
                }),
            });
        }
    }
    async fn close(&mut self) {
        self.stop_qr().await;
        if let Some(job) = self.action_job.take() {
            job.stop().await;
        }
        self.stop_session().await;
    }
}
pub async fn run(
    adb: Adb,
    path: PathBuf,
    mut commands: mpsc::Receiver<Result<Value>>,
    output: watch::Sender<String>,
    cancel: Cancel,
) -> Result<()> {
    let tracker_cancel = Cancel::new();
    let (tc, dc) = (tracker_cancel.clone(), tracker_cancel.clone());
    let (device_tx, devices) = watch::channel(vec![]);
    let (error_tx, errors) = watch::channel(None);
    let tracking_adb = adb.clone();
    let tracker =
        tokio::spawn(async move { adb::track(tracking_adb, device_tx, error_tx, tc).await });
    let (clipboard, mut wayland_task) = wayland::start(dc);
    let mut daemon = Daemon::new(adb, path, devices, errors, clipboard);
    let mut last = None;
    let result=async {
        loop {
            daemon.reconcile().await;
            let status=daemon.status();
            if last.as_ref()!=Some(&status) {
                output.send_replace(serde_json::to_string(&status).map_err(|_|"Cannot encode status")?);last=Some(status);
            }
            let deadline=daemon.retry;
            tokio::select! {
                biased;
                _=cancel.cancelled()=>return Ok(()),
                result=&mut wayland_task=>return result.unwrap_or(Err("Wayland task stopped")),
                command=commands.recv()=>match command {
                    Some(Ok(value))=>if let Err(e)=daemon.command(value).await {daemon.action=e.into();},
                    Some(Err(e))=>daemon.action=e.into(),
                    None=>return Ok(()),
                },
                _=daemon.devices.changed()=>{},
                _=daemon.errors.changed()=>{},
                _=daemon.ready.changed()=>if *daemon.ready.borrow_and_update() {
                    daemon.state="syncing".into();daemon.message="Sharing new plain-text copies".into();daemon.delay=1;
                },
                _=daemon.qr_status.changed()=>{daemon.action=daemon.qr_status.borrow_and_update().message.clone();},
                _=daemon.action_result.changed()=>{daemon.action=daemon.action_result.borrow_and_update().clone();},
                result=async {match &mut daemon.session {Some(j)=>(&mut j.handle).await,None=>std::future::pending().await}},if daemon.session.is_some()=>{
                    daemon.session=None;daemon.session_key=None;daemon.state="waiting".into();
                    daemon.clipboard.suspend().await;
                    daemon.message=result.unwrap_or(Err("Clipboard session stopped; reconnect phone")).err().unwrap_or("Clipboard session ended; reconnecting").into();
                    daemon.delay=(daemon.delay*2).min(15);daemon.retry=Some(Instant::now()+Duration::from_secs(daemon.delay));
                },
                _=async {match deadline {Some(d)=>tokio::time::sleep_until(d).await,None=>std::future::pending().await}}=>{daemon.retry=None;}
            }
        }
    }.await;
    daemon.close().await;
    tracker_cancel.cancel();
    let _ = tracker.await;
    if !wayland_task.is_finished() {
        let _ = wayland_task.await;
    }
    result
}

//! Native wlr-data-control-v1, one seat, regular selection only.
//! All Wayland dispatch and ownership changes run on the single reactor thread.
use crate::{
    Cancel, Result,
    protocol::{MAX_TEXT, valid},
};
use std::{
    collections::HashMap,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, unix::AsyncFd},
    net::UnixStream,
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_callback, wl_registry, wl_seat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as device, zwlr_data_control_manager_v1 as manager,
    zwlr_data_control_offer_v1 as offer, zwlr_data_control_source_v1 as source,
};
const PLAIN: [&str; 4] = [
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "text/plain",
    "UTF8_STRING",
];
const SENSITIVE: &str = "x-kde-passwordManagerHint";
#[derive(Clone, Default)]
pub struct Snapshot {
    pub revision: u64,
    pub data: Option<Arc<[u8]>>,
    pub own: bool,
    pub ready: bool,
}
impl Snapshot {
    pub fn bytes(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }
}
pub enum Request {
    Suspend(oneshot::Sender<()>),
    Baseline(oneshot::Sender<Result<Snapshot>>),
    Write {
        revision: u64,
        data: Arc<[u8]>,
        reply: oneshot::Sender<Result<bool>>,
    },
}
#[derive(Clone)]
pub struct Clipboard {
    pub events: watch::Receiver<Snapshot>,
    pub requests: mpsc::Sender<Request>,
}
impl Clipboard {
    pub async fn suspend(&self) {
        let (tx, rx) = oneshot::channel();
        if self.requests.send(Request::Suspend(tx)).await.is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(3), rx).await;
        }
    }

    pub async fn baseline(&self) -> Result<Snapshot> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Request::Baseline(tx))
            .await
            .map_err(|_| "Wayland clipboard closed")?;
        tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .map_err(|_| "Wayland baseline timed out")?
            .map_err(|_| "Wayland clipboard closed")?
    }
    pub async fn write(&self, revision: u64, data: Arc<[u8]>) -> Result<bool> {
        if !valid(&data) {
            return Err("Invalid clipboard text");
        }
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(Request::Write {
                revision,
                data,
                reply: tx,
            })
            .await
            .map_err(|_| "Wayland clipboard closed")?;
        tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .map_err(|_| "Wayland clipboard write timed out")?
            .map_err(|_| "Wayland clipboard closed")?
    }
}
enum Callback {
    Globals,
    Initialized,
    Request(Mutex<Option<Request>>),
}
struct State {
    manager: Option<manager::ZwlrDataControlManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    device: Option<device::ZwlrDataControlDeviceV1>,
    offers: HashMap<wayland_client::backend::ObjectId, Vec<String>>,
    source: Option<source::ZwlrDataControlSourceV1>,
    selected: Option<offer::ZwlrDataControlOfferV1>,
    monitoring: bool,
    snapshot: Snapshot,
    tx: watch::Sender<Snapshot>,
    transfer: Option<JoinHandle<()>>,
    complete: mpsc::Sender<(u64, Option<Arc<[u8]>>)>,
    sends: JoinSet<()>,
    baselines: Vec<oneshot::Sender<Result<Snapshot>>>,
    initialized: bool,
    failed: Option<&'static str>,
    marker: String,
    probe: bool,
    globals: Vec<String>,
}
impl State {
    fn publish(&mut self) {
        self.tx.send_replace(self.snapshot.clone());
        if self.initialized && self.snapshot.ready {
            for reply in self.baselines.drain(..) {
                let _ = reply.send(Ok(self.snapshot.clone()));
            }
        }
    }
    fn receive(&mut self, selected: Option<offer::ZwlrDataControlOfferV1>) {
        if self.selected != selected {
            if let Some(old) = self.selected.take() {
                self.offers.remove(&old.id());
                old.destroy();
            }
            self.selected = selected.clone();
        }
        if let Some(task) = self.transfer.take() {
            task.abort();
        }
        self.snapshot = Snapshot {
            revision: self.snapshot.revision + 1,
            ready: true,
            ..Snapshot::default()
        };
        if let Some(offer) = selected {
            let types = self.offers.get(&offer.id()).cloned().unwrap_or_default();
            if types.contains(&self.marker) {
                self.snapshot.own = true;
                self.snapshot.data = self
                    .source
                    .as_ref()
                    .and_then(|s| s.data::<Arc<[u8]>>())
                    .cloned();
            } else if self.monitoring
                && !types.iter().any(|m| m == SENSITIVE)
                && let Some(mime) = PLAIN.iter().find(|m| types.iter().any(|t| t == *m))
            {
                match std::os::unix::net::UnixStream::pair() {
                    Ok((read, write)) => {
                        if read.set_nonblocking(true).is_err() {
                            self.failed = Some("Cannot open clipboard transfer");
                            return;
                        }
                        let Ok(mut read) = UnixStream::from_std(read) else {
                            self.failed = Some("Cannot register clipboard transfer");
                            return;
                        };
                        offer.receive((*mime).into(), write.as_fd());
                        drop(write);
                        self.snapshot.ready = false;
                        let revision = self.snapshot.revision;
                        let tx = self.complete.clone();
                        self.transfer = Some(tokio::spawn(async move {
                            let mut data = Vec::new();
                            let result = tokio::time::timeout(
                                Duration::from_secs(2),
                                (&mut read).take(MAX_TEXT as u64 + 1).read_to_end(&mut data),
                            )
                            .await;
                            let data = if matches!(result, Ok(Ok(_))) && valid(&data) {
                                Some(Arc::from(data))
                            } else {
                                None
                            };
                            let _ = tx.send((revision, data)).await;
                        }));
                    }
                    Err(_) => self.failed = Some("Cannot open clipboard transfer"),
                }
            }
        }
        self.publish();
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        r: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            s.globals.push(format!("{interface} v{version}"));
            if s.probe {
                return;
            }
            match interface.as_str() {
                "zwlr_data_control_manager_v1" if s.manager.is_none() => {
                    s.manager = Some(r.bind(name, version.min(2), q, ()))
                }
                "wl_seat" if s.seat.is_none() => s.seat = Some(r.bind(name, version.min(2), q, ())),
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_callback::WlCallback, Callback> for State {
    fn event(
        s: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        data: &Callback,
        c: &Connection,
        q: &QueueHandle<Self>,
    ) {
        match data {
            Callback::Globals => {
                if s.probe {
                    s.initialized = true;
                    return;
                }
                if let (Some(manager), Some(seat)) = (&s.manager, &s.seat) {
                    s.device = Some(manager.get_data_device(seat, q, ()));
                    c.display().sync(q, Callback::Initialized);
                } else {
                    s.failed = Some(
                        "Compositor lacks wlr-data-control or a seat; use supported Hyprland and restart the bridge",
                    );
                }
            }
            Callback::Initialized => {
                s.initialized = true;
                s.publish();
            }
            Callback::Request(request) => {
                if let Some(request) = request.lock().unwrap().take() {
                    match request {
                        Request::Baseline(reply) => {
                            s.baselines.push(reply);
                            s.monitoring = true;
                            s.receive(s.selected.clone());
                        }
                        Request::Suspend(reply) => {
                            s.monitoring = false;
                            s.receive(s.selected.clone());
                            let _ = reply.send(());
                        }
                        Request::Write {
                            revision,
                            data,
                            reply,
                        } => {
                            if revision != s.snapshot.revision
                                || !s.snapshot.ready
                                || reply.is_closed()
                            {
                                let _ = reply.send(Ok(false));
                                return;
                            }
                            let src = s.manager.as_ref().unwrap().create_data_source(q, data);
                            for mime in PLAIN {
                                src.offer(mime.into());
                            }
                            src.offer(SENSITIVE.into());
                            src.offer(s.marker.clone());
                            s.device.as_ref().unwrap().set_selection(Some(&src));
                            if let Some(old) = s.source.replace(src) {
                                old.destroy();
                            }
                            let _ = reply.send(Ok(true));
                        }
                    }
                }
            }
        }
    }
}
impl Dispatch<device::ZwlrDataControlDeviceV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &device::ZwlrDataControlDeviceV1,
        e: device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            device::Event::DataOffer { id } => {
                s.offers.insert(id.id(), vec![]);
            }
            device::Event::Selection { id } => s.receive(id),
            device::Event::PrimarySelection { id: Some(o) } => {
                s.offers.remove(&o.id());
                o.destroy();
            }
            device::Event::Finished => {
                s.failed = Some("Wayland clipboard access ended; restart the bridge")
            }
            _ => {}
        }
    }
    wayland_client::event_created_child!(State,device::ZwlrDataControlDeviceV1,[0=>(offer::ZwlrDataControlOfferV1,())]);
}
impl Dispatch<offer::ZwlrDataControlOfferV1, ()> for State {
    fn event(
        s: &mut Self,
        o: &offer::ZwlrDataControlOfferV1,
        e: offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let offer::Event::Offer { mime_type } = e {
            let types = s.offers.entry(o.id()).or_default();
            // Store only MIME types relevant to policy; never unbounded offers.
            if (PLAIN.contains(&mime_type.as_str())
                || mime_type == SENSITIVE
                || mime_type == s.marker)
                && !types.contains(&mime_type)
            {
                types.push(mime_type);
            }
        }
    }
}
impl Dispatch<source::ZwlrDataControlSourceV1, Arc<[u8]>> for State {
    fn event(
        s: &mut Self,
        src: &source::ZwlrDataControlSourceV1,
        e: source::Event,
        data: &Arc<[u8]>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            source::Event::Send { mime_type, fd } => {
                if s.sends.len() >= 16 {
                    return;
                }
                let bytes: Arc<[u8]> = if PLAIN.contains(&mime_type.as_str()) {
                    data.clone()
                } else if mime_type == SENSITIVE {
                    Arc::from(&b"secret"[..])
                } else {
                    Arc::from([])
                };
                s.sends.spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(3), send_fd(fd, &bytes)).await;
                });
            }
            source::Event::Cancelled => {
                if s.source.as_ref() == Some(src) {
                    s.source = None;
                }
                src.destroy();
            }
            _ => {}
        }
    }
}
wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(State: ignore manager::ZwlrDataControlManagerV1);
async fn send_fd(fd: OwnedFd, bytes: &[u8]) -> std::io::Result<()> {
    // The received fd may be a pipe, not a socket; use readiness + write(2).
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let fd = AsyncFd::new(fd)?;
    let mut offset = 0;
    while offset < bytes.len() {
        let mut ready = fd.writable().await?;
        let result = ready.try_io(|fd| {
            let n = unsafe {
                libc::write(
                    fd.get_ref().as_raw_fd(),
                    bytes[offset..].as_ptr().cast(),
                    bytes.len() - offset,
                )
            };
            if n < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        });
        if let Ok(n) = result {
            let n = n?;
            if n == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            offset += n;
        }
    }
    Ok(())
}
pub fn start(cancel: Cancel) -> (Clipboard, JoinHandle<Result<()>>) {
    let (tx, events) = watch::channel(Snapshot::default());
    let (requests, rx) = mpsc::channel(4);
    let task = tokio::spawn(async move { run(tx, rx, cancel, false).await.map(|_| ()) });
    (Clipboard { events, requests }, task)
}
pub async fn probe() -> Result<Vec<String>> {
    let (tx, _) = watch::channel(Snapshot::default());
    let (_rq, rx) = mpsc::channel(1);
    tokio::time::timeout(Duration::from_secs(3), run(tx, rx, Cancel::new(), true))
        .await
        .map_err(|_| "Wayland registry probe timed out")?
}
async fn run(
    tx: watch::Sender<Snapshot>,
    mut requests: mpsc::Receiver<Request>,
    cancel: Cancel,
    probe: bool,
) -> Result<Vec<String>> {
    let conn = Connection::connect_to_env()
        .map_err(|_| "Wayland display unavailable; restart inside your graphical session")?;
    let mut queue = conn.new_event_queue();
    let q = queue.handle();
    let (complete, mut transfers) = mpsc::channel(4);
    let mut s = State {
        manager: None,
        seat: None,
        device: None,
        offers: HashMap::new(),
        source: None,
        selected: None,
        monitoring: false,
        snapshot: Snapshot::default(),
        tx,
        transfer: None,
        complete,
        sends: JoinSet::new(),
        baselines: vec![],
        initialized: false,
        failed: None,
        marker: format!("application/x-omaclip-{}", crate::random_hex(16)?),
        probe,
        globals: vec![],
    };
    conn.display().get_registry(&q, ());
    conn.display().sync(&q, Callback::Globals);
    let fd = AsyncFd::new(
        conn.backend()
            .poll_fd()
            .try_clone_to_owned()
            .map_err(|_| "Cannot register Wayland socket")?,
    )
    .map_err(|_| "Cannot register Wayland socket")?;
    let result=async {
        loop {
            queue.dispatch_pending(&mut s).map_err(|_|"Wayland dispatch failed; restart bridge")?;
            if let Some(error)=s.failed {return Err(error);}
            if probe && s.initialized {return Ok(s.globals.clone());}
            match conn.flush() {
                Ok(())=>{},
                Err(wayland_client::backend::WaylandError::Io(e)) if e.kind()==std::io::ErrorKind::WouldBlock=>{
                    tokio::select! {_=cancel.cancelled()=>return Ok(vec![]), result=fd.writable()=>{result.map_err(|_|"Wayland socket closed")?.clear_ready();}}
                    continue;
                }
                Err(_)=>return Err("Wayland socket closed; restart bridge"),
            }
            let Some(guard)=queue.prepare_read() else {continue;};
            tokio::select! {
                biased;
                _=cancel.cancelled()=>return Ok(vec![]),
                ready=fd.readable()=>{
                    let mut ready=ready.map_err(|_|"Wayland socket closed")?;
                    match guard.read() {
                        Ok(_)=>{},
                        Err(wayland_client::backend::WaylandError::Io(e)) if e.kind()==std::io::ErrorKind::WouldBlock=>{},
                        Err(_)=>return Err("Wayland connection closed"),
                    }
                    ready.clear_ready();
                }
                Some((revision,data))=transfers.recv()=>{
                    drop(guard);
                    if revision==s.snapshot.revision {s.snapshot.data=data;s.snapshot.ready=true;s.publish();}
                }
                Some(request)=requests.recv(), if s.initialized=>{
                    drop(guard);conn.display().sync(&q,Callback::Request(Mutex::new(Some(request))));
                }
                Some(_)=s.sends.join_next(),if !s.sends.is_empty()=>{drop(guard);}
            }
        }
    }.await;
    if let Some(task) = s.transfer.take() {
        task.abort();
        let _ = task.await;
    }
    s.sends.abort_all();
    while s.sends.join_next().await.is_some() {}
    if let Some(src) = s.source.take() {
        src.destroy();
    }
    if let Some(d) = s.device.take() {
        d.destroy();
    }
    let _ = conn.flush();
    result
}

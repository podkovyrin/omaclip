pub mod adb;
pub mod config;
pub mod daemon;
pub mod pairing;
pub mod process;
pub mod protocol;
pub mod session;
pub mod sync;
pub mod transport;
pub mod wayland;

// Errors are deliberately static: never expose subprocess diagnostics or payloads.
pub type Result<T> = std::result::Result<T, &'static str>;
#[derive(Clone, Default)]
pub struct Cancel(tokio::sync::watch::Sender<bool>);
impl Cancel {
    pub fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}
pub fn random_hex(n: usize) -> Result<String> {
    use std::io::Read;
    let mut bytes = vec![0; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| "System randomness unavailable")?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}

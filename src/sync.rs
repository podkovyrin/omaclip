use crate::protocol::valid;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};
type Hash = [u8; 32];
fn digest(data: Option<&[u8]>) -> Option<Hash> {
    data.map(|v| Sha256::digest(v).into())
}
// Epoch-local digests. No payload history and no state survives pause/reconnect.
pub struct Policy {
    desktop: Option<Hash>,
    echoes: VecDeque<(Hash, Instant)>,
}
impl Policy {
    pub fn new(baseline: Option<&[u8]>) -> Self {
        Self {
            desktop: digest(baseline),
            echoes: VecDeque::with_capacity(64),
        }
    }
    pub fn desktop(&mut self, data: Option<&[u8]>) -> bool {
        let next = digest(data);
        if next == self.desktop {
            return false;
        }
        self.desktop = next;
        data.is_some_and(valid)
    }
    // Record only after a successful wire write.
    pub fn sent(&mut self, data: &[u8]) {
        if self.echoes.len() == 64 {
            self.echoes.pop_front();
        }
        self.echoes.push_back((
            digest(Some(data)).unwrap(),
            Instant::now() + Duration::from_secs(3),
        ));
    }
    pub fn phone(&mut self, data: &[u8]) -> bool {
        if !valid(data) {
            return false;
        }
        while self
            .echoes
            .front()
            .is_some_and(|(_, until)| *until < Instant::now())
        {
            self.echoes.pop_front();
        }
        let hash = digest(Some(data)).unwrap();
        Some(hash) != self.desktop && !self.echoes.iter().any(|(h, _)| *h == hash)
    }
    pub fn applied(&mut self, data: &[u8]) {
        self.desktop = digest(Some(data));
    }
}

use crate::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub serial: String,
    pub identity: String,
    pub enabled: bool,
}
impl Config {
    pub fn path() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            })
            .join("omaclip/state.json")
    }
    pub fn load(path: &std::path::Path) -> Self {
        let bytes = (|| -> std::io::Result<Vec<u8>> {
            use std::io::Read;
            let mut bytes = Vec::new();
            fs::File::open(path)?.take(8193).read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        let mut value: Self = bytes
            .ok()
            .filter(|b| b.len() <= 8192)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if value.serial.len() > 512 || value.identity.len() > 256 {
            return Self::default();
        }
        value.enabled &= !value.serial.is_empty() && !value.identity.is_empty();
        value
    }
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let save = || -> std::io::Result<()> {
            fs::create_dir_all(path.parent().unwrap())?;
            let temp = path.with_extension("tmp");
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&serde_json::to_vec(self)?)?;
            file.sync_all()?;
            fs::rename(temp, path)
        };
        save().map_err(|_| "Cannot save selection; check configuration directory permissions")
    }
}

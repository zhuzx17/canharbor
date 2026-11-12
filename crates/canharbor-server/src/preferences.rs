// SPDX-License-Identifier: AGPL-3.0-or-later
//! User-interface preferences, independent of runtime sockets and CAN state.
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    sync::Mutex,
};

pub use canharbor_api::{UiLocale as Locale, UiPreferences as Preferences};

pub fn default_path() -> Option<PathBuf> {
    let root = if let Some(path) = std::env::var_os("CANHARBOR_CONFIG_DIR") {
        PathBuf::from(path)
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("CANHarbor")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/CANHarbor")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            })
            .join("canharbor")
    };
    root.is_absolute().then(|| root.join("preferences.json"))
}

pub struct Store {
    file: Option<PathBuf>,
    value: Mutex<Preferences>,
}
impl Store {
    pub fn new(file: Option<PathBuf>) -> Self {
        let value = file
            .as_ref()
            .and_then(|p| {
                let mut f = canharbor_platform::private::open_read(p).ok()?;
                if f.metadata().ok()?.len() > 128 {
                    return None;
                }
                let mut bytes = Vec::new();
                std::io::Read::by_ref(&mut f)
                    .take(129)
                    .read_to_end(&mut bytes)
                    .ok()?;
                serde_json::from_slice(&bytes).ok()
            })
            .unwrap_or_default();
        Self {
            file,
            value: Mutex::new(value),
        }
    }
    pub fn get(&self) -> Preferences {
        *self.value.lock().unwrap()
    }
    pub fn set(&self, next: Preferences) -> io::Result<()> {
        let mut value = self.value.lock().unwrap();
        if let Some(file) = &self.file {
            let root = file
                .parent()
                .ok_or_else(|| io::Error::other("missing settings directory"))?;
            canharbor_platform::private::directory(root)?;
            let temporary = root.join(format!(".preferences-{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| {
                let mut output = canharbor_platform::private::create_new(&temporary)?;
                output.write_all(&serde_json::to_vec(&next)?)?;
                output.sync_all()?;
                canharbor_platform::private::replace(&temporary, file)?;
                canharbor_platform::private::sync_directory(root)
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result?;
        }
        *value = next;
        Ok(())
    }
}

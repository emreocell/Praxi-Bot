//! Persistent list of paired devices.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use iroh::{EndpointAddr, PublicKey};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::{PeerError, Result};
use crate::identity::{key_from_string, key_to_string};

/// What a paired peer is, from the point of view of the side that stored it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A remote machine that is controlled (runs `bot-node`).
    Node,
    /// The companion phone app.
    Phone,
    /// The desktop app / core (the controlling side).
    Desktop,
}

impl Role {
    /// Whether peers of this role may issue `desktop.*` commands.
    #[must_use]
    pub fn may_control(self) -> bool {
        matches!(self, Role::Desktop | Role::Phone)
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Role::Node => "node",
            Role::Phone => "phone",
            Role::Desktop => "desktop",
        })
    }
}

impl std::str::FromStr for Role {
    type Err = PeerError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "node" => Ok(Role::Node),
            "phone" => Ok(Role::Phone),
            "desktop" => Ok(Role::Desktop),
            other => Err(PeerError::Protocol(format!(
                "unknown role '{other}' (expected node, phone or desktop)"
            ))),
        }
    }
}

/// A paired peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Public key (hex), the peer's identity.
    pub id: String,
    /// Human-readable name chosen by the peer.
    pub name: String,
    /// The peer's role.
    pub role: Role,
    /// Capabilities the peer advertised (e.g. `desktop`, `browser`).
    pub capabilities: Vec<String>,
    /// Last known dialable address.
    pub addr: Option<EndpointAddr>,
    /// Unix seconds when pairing completed.
    pub paired_at: u64,
}

impl Device {
    /// The parsed public key.
    ///
    /// # Errors
    ///
    /// Returns an error if the stored id is not a valid key.
    pub fn public_key(&self) -> Result<PublicKey> {
        key_from_string(&self.id)
    }
}

/// Current unix time in seconds.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    version: u32,
    devices: Vec<Device>,
}

/// In-memory device list persisted to a JSON file on every change.
pub struct DeviceStore {
    path: PathBuf,
    devices: Mutex<Vec<Device>>,
}

fn io(e: &impl std::fmt::Display) -> PeerError {
    PeerError::Storage(e.to_string())
}

impl DeviceStore {
    /// Open (or create empty) the store at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if an existing file cannot be read or parsed.
    pub fn open(path: PathBuf) -> Result<Self> {
        let devices = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str::<File>(&text)
                    .map_err(|e| io(&e))?
                    .devices
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(io(&e)),
        };
        Ok(Self {
            path,
            devices: Mutex::new(devices),
        })
    }

    fn persist(&self, devices: &[Device]) -> Result<()> {
        let file = File {
            version: 1,
            devices: devices.to_vec(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| io(&e))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| io(&e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| io(&e))
    }

    /// All paired devices.
    #[must_use]
    pub fn list(&self) -> Vec<Device> {
        self.devices.lock().clone()
    }

    /// Look up a device by key.
    #[must_use]
    pub fn get(&self, key: &PublicKey) -> Option<Device> {
        let id = key_to_string(key);
        self.devices.lock().iter().find(|d| d.id == id).cloned()
    }

    /// Insert or replace a device.
    ///
    /// # Errors
    ///
    /// Returns an error if the list cannot be persisted.
    pub fn upsert(&self, device: Device) -> Result<()> {
        let mut guard = self.devices.lock();
        match guard.iter_mut().find(|d| d.id == device.id) {
            Some(slot) => *slot = device,
            None => guard.push(device),
        }
        self.persist(&guard)
    }

    /// Update a device's last known address; returns `false` if unknown.
    ///
    /// # Errors
    ///
    /// Returns an error if the list cannot be persisted.
    pub fn update_addr(&self, key: &PublicKey, addr: EndpointAddr) -> Result<bool> {
        let id = key_to_string(key);
        let mut guard = self.devices.lock();
        let Some(dev) = guard.iter_mut().find(|d| d.id == id) else {
            return Ok(false);
        };
        if dev.addr.as_ref() != Some(&addr) {
            dev.addr = Some(addr);
            self.persist(&guard)?;
        }
        Ok(true)
    }

    /// Remove a device; returns whether it existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the list cannot be persisted.
    pub fn remove(&self, key: &PublicKey) -> Result<bool> {
        let id = key_to_string(key);
        let mut guard = self.devices.lock();
        let before = guard.len();
        guard.retain(|d| d.id != id);
        let removed = guard.len() != before;
        if removed {
            self.persist(&guard)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(key: &PublicKey, role: Role) -> Device {
        Device {
            id: key_to_string(key),
            name: "x".into(),
            role,
            capabilities: vec!["desktop".into()],
            addr: None,
            paired_at: 1,
        }
    }

    #[test]
    fn persists_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let k = iroh::SecretKey::generate().public();
        {
            let s = DeviceStore::open(path.clone()).unwrap();
            s.upsert(dev(&k, Role::Node)).unwrap();
        }
        let s = DeviceStore::open(path).unwrap();
        assert_eq!(s.get(&k).unwrap().role, Role::Node);
        assert!(s.remove(&k).unwrap());
        assert!(s.get(&k).is_none());
        assert!(!s.remove(&k).unwrap());
    }

    #[test]
    fn role_parsing() {
        assert_eq!("phone".parse::<Role>().unwrap(), Role::Phone);
        assert!("root".parse::<Role>().is_err());
    }
}

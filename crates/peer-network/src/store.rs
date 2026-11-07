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
    /// An always-on daemon server.
    Host,
}

impl Role {
    /// Whether this is the `node` role.
    #[must_use]
    pub fn is_node(self) -> bool {
        self == Self::Node
    }
    /// Whether this is the `phone` role.
    #[must_use]
    pub fn is_phone(self) -> bool {
        self == Self::Phone
    }
    /// Whether this is the `desktop` role.
    #[must_use]
    pub fn is_desktop(self) -> bool {
        self == Self::Desktop
    }
    /// Whether this is the `host` role.
    #[must_use]
    pub fn is_host(self) -> bool {
        self == Self::Host
    }

    /// Whether peers of this role may issue `desktop.*` commands.
    #[must_use]
    pub fn may_control(self) -> bool {
        matches!(self, Role::Desktop | Role::Phone | Role::Host)
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Role::Node => "node",
            Role::Phone => "phone",
            Role::Desktop => "desktop",
            Role::Host => "host",
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
            "host" => Ok(Role::Host),
            other => Err(PeerError::Protocol(format!(
                "unknown role '{other}' (expected node, phone, desktop or host)"
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
    /// The replacement list is committed with the temp-file rename before
    /// memory changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the list cannot be persisted. Memory is unchanged.
    pub fn upsert(&self, device: Device) -> Result<()> {
        let mut guard = self.devices.lock();
        let mut next = guard.clone();
        match next.iter_mut().find(|d| d.id == device.id) {
            Some(slot) => *slot = device,
            None => next.push(device),
        }
        self.persist(&next)?;
        *guard = next;
        Ok(())
    }

    /// Update a device's last known address; returns `false` if unknown.
    ///
    /// The replacement list is committed with the temp-file rename before
    /// memory changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the list cannot be persisted. Memory is unchanged.
    pub fn update_addr(&self, key: &PublicKey, addr: EndpointAddr) -> Result<bool> {
        let id = key_to_string(key);
        let mut guard = self.devices.lock();
        let Some(index) = guard.iter().position(|d| d.id == id) else {
            return Ok(false);
        };
        if guard[index].addr.as_ref() == Some(&addr) {
            return Ok(true);
        }
        let mut next = guard.clone();
        next[index].addr = Some(addr);
        self.persist(&next)?;
        *guard = next;
        Ok(true)
    }

    /// Remove a device; returns whether it existed.
    ///
    /// The replacement list is committed with the temp-file rename before
    /// memory changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the list cannot be persisted. Memory is unchanged.
    pub fn remove(&self, key: &PublicKey) -> Result<bool> {
        let id = key_to_string(key);
        let mut guard = self.devices.lock();
        if !guard.iter().any(|d| d.id == id) {
            return Ok(false);
        }
        let mut next = guard.clone();
        next.retain(|d| d.id != id);
        self.persist(&next)?;
        *guard = next;
        Ok(true)
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
    fn host_round_trip_and_legacy_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let key = iroh::SecretKey::generate().public();
        // The original version-1 file shape and role remain readable.
        let legacy = serde_json::json!({"version":1,"devices":[{
            "id":key_to_string(&key),"name":"old node","role":"node",
            "capabilities":[],"addr":null,"paired_at":1
        }]});
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let store = DeviceStore::open(path.clone()).unwrap();
        assert!(store.get(&key).unwrap().role.is_node());
        store.upsert(dev(&key, Role::Host)).unwrap();
        drop(store);
        assert!(
            DeviceStore::open(path)
                .unwrap()
                .get(&key)
                .unwrap()
                .role
                .is_host()
        );
        for role in [Role::Node, Role::Phone, Role::Desktop, Role::Host] {
            assert_eq!(role.to_string().parse::<Role>().unwrap(), role);
        }
        assert!(Role::Phone.is_phone());
        assert!(Role::Desktop.is_desktop());
        assert!(Role::Host.may_control());
        assert!(!Role::Node.may_control());
    }

    #[test]
    fn role_parsing() {
        assert_eq!("phone".parse::<Role>().unwrap(), Role::Phone);
        assert!("root".parse::<Role>().is_err());
    }

    #[test]
    fn injected_persist_failure_does_not_commit_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let store = DeviceStore::open(path.clone()).unwrap();
        let kept = iroh::SecretKey::generate().public();
        let added = iroh::SecretKey::generate().public();
        store.upsert(dev(&kept, Role::Node)).unwrap();
        let before = std::fs::read(&path).unwrap();
        // `persist` writes `devices.json.tmp` and then renames it into place.
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();

        let err = store.upsert(dev(&added, Role::Phone)).unwrap_err();
        assert!(matches!(err, PeerError::Storage(_)), "{err}");
        assert!(store.get(&added).is_none());
        assert_eq!(store.get(&kept).unwrap().role, Role::Node);
        assert_eq!(store.get(&kept).unwrap().name, "x");
        let mut changed = dev(&kept, Role::Host);
        changed.name = "changed".into();
        let err = store.upsert(changed).unwrap_err();
        assert!(matches!(err, PeerError::Storage(_)), "{err}");
        assert_eq!(store.get(&kept).unwrap().role, Role::Node);
        assert_eq!(store.get(&kept).unwrap().name, "x");
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let err = store
            .update_addr(&kept, EndpointAddr::new(kept))
            .unwrap_err();
        assert!(matches!(err, PeerError::Storage(_)), "{err}");
        assert!(store.get(&kept).unwrap().addr.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let err = store.remove(&kept).unwrap_err();
        assert!(matches!(err, PeerError::Storage(_)), "{err}");
        assert!(store.get(&kept).is_some());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

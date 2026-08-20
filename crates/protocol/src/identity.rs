use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct StableIdentity(pub [u8; 16]);

impl StableIdentity {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    pub fn hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PairingRecord {
    pub host_identity: StableIdentity,
    pub host_name: String,
    pub address: String,
    pub control_port: u16,
}

#[derive(Debug)]
pub struct PairingStore {
    path: PathBuf,
    records: Vec<PairingRecord>,
}

impl PairingStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let records = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                format!("cannot decode pairing store {}: {error}", path.display())
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(format!(
                    "cannot read pairing store {}: {error}",
                    path.display()
                ));
            }
        };
        Ok(Self { path, records })
    }

    pub fn records(&self) -> &[PairingRecord] {
        &self.records
    }

    pub fn remember(&mut self, record: PairingRecord) -> Result<(), String> {
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|existing| existing.host_identity == record.host_identity)
        {
            *existing = record;
        } else {
            self.records.push(record);
        }
        self.persist()
    }

    pub fn forget(&mut self, identity: StableIdentity) -> Result<bool, String> {
        let before = self.records.len();
        self.records
            .retain(|record| record.host_identity != identity);
        if self.records.len() == before {
            return Ok(false);
        }
        self.persist()?;
        Ok(true)
    }

    fn persist(&self) -> Result<(), String> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "cannot create pairing directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let bytes = serde_json::to_vec_pretty(&self.records)
            .map_err(|error| format!("cannot encode pairing store: {error}"))?;
        fs::write(&self.path, bytes).map_err(|error| {
            format!(
                "cannot write pairing store {}: {error}",
                self.path.display()
            )
        })
    }
}

pub fn default_pairing_path(config_dir: &Path) -> PathBuf {
    config_dir.join("pairings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(identity: StableIdentity) -> PairingRecord {
        PairingRecord {
            host_identity: identity,
            host_name: "test-host".to_owned(),
            address: "192.0.2.10".to_owned(),
            control_port: 5005,
        }
    }

    #[test]
    fn remembered_pairing_replaces_the_same_host_without_duplicates() {
        let path =
            std::env::temp_dir().join(format!("lanplay-pairing-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let identity = StableIdentity([7; 16]);
        let mut store = PairingStore::load(&path).expect("empty store loads");
        store.remember(record(identity)).expect("record persists");
        let mut updated = record(identity);
        updated.address = "192.0.2.11".to_owned();
        store.remember(updated).expect("record updates");
        assert_eq!(store.records().len(), 1);
        assert_eq!(store.records()[0].address, "192.0.2.11");
        let loaded = PairingStore::load(&path).expect("persisted store loads");
        assert_eq!(loaded.records(), store.records());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn forgetting_an_unknown_host_is_a_noop() {
        let path =
            std::env::temp_dir().join(format!("lanplay-pairing-empty-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut store = PairingStore::load(&path).expect("empty store loads");
        assert!(
            !store
                .forget(StableIdentity([8; 16]))
                .expect("forget succeeds")
        );
    }
}

// Michael Burgus — https://github.com/NeuralDrifter
// Single-instance lock shared by the Simple and Advanced editions

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

const LOCK_FILE: &str = "instance.lock";
const INFO_FILE: &str = "instance.json";

/// Holds the OS file lock for as long as it lives. The OS releases it on exit or crash.
pub struct InstanceLock {
    _file: File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolderInfo {
    pub edition: String,
    pub pid: u32,
    pub started_at: String,
}

pub enum Acquire {
    Acquired(InstanceLock),
    /// Another process holds the lock. Info is None if instance.json was unreadable.
    HeldBy(Option<HolderInfo>),
}

pub fn try_acquire(data_dir: &Path, edition: &str) -> std::io::Result<Acquire> {
    std::fs::create_dir_all(data_dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data_dir.join(LOCK_FILE))?;

    match file.try_lock() {
        Ok(()) => {
            // instance.json is a separate file: a locked file can't be read by others on Windows
            let info = HolderInfo {
                edition: edition.to_string(),
                pid: std::process::id(),
                started_at: unix_now().to_string(),
            };
            let json = serde_json::to_string(&info).map_err(std::io::Error::other)?;
            std::fs::write(data_dir.join(INFO_FILE), json)?;
            Ok(Acquire::Acquired(InstanceLock { _file: file }))
        }
        Err(TryLockError::WouldBlock) => {
            let info = std::fs::read_to_string(data_dir.join(INFO_FILE))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());
            Ok(Acquire::HeldBy(info))
        }
        Err(TryLockError::Error(e)) => Err(e),
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_first_held() {
        let dir = tempfile::tempdir().unwrap();

        let first = match try_acquire(dir.path(), "Simple").unwrap() {
            Acquire::Acquired(l) => l,
            Acquire::HeldBy(_) => panic!("first acquire should succeed"),
        };

        match try_acquire(dir.path(), "Advanced").unwrap() {
            Acquire::Acquired(_) => panic!("second acquire should fail while first is held"),
            Acquire::HeldBy(info) => assert_eq!(info.unwrap().edition, "Simple"),
        }

        drop(first);
        assert!(matches!(
            try_acquire(dir.path(), "Advanced").unwrap(),
            Acquire::Acquired(_)
        ));
    }

    #[test]
    fn creates_lock_and_info_files() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("nested");
        let _lock = try_acquire(&data, "Advanced").unwrap();
        assert!(data.join(LOCK_FILE).exists());
        assert!(data.join(INFO_FILE).exists());
    }
}

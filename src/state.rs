use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub volume_identity: String,
    pub journal_id: u64,
    pub next_usn: i64,
    pub backfill_end: i64,
    pub caught_up: bool,
}

impl Checkpoint {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 1, "unsupported checkpoint schema");
        ensure!(
            !self.volume_identity.is_empty(),
            "empty checkpoint volume identity"
        );
        ensure!(
            self.next_usn >= 0 && self.backfill_end >= 0,
            "negative checkpoint USN"
        );
        Ok(())
    }

    pub fn validate_live(&self) -> Result<()> {
        self.validate()?;
        ensure!(
            self.next_usn >= self.backfill_end && self.caught_up,
            "invalid live checkpoint: cursor precedes its split boundary or lane is not live"
        );
        Ok(())
    }
}

pub trait Store {
    fn save(&mut self, checkpoint: &Checkpoint) -> Result<()>;
}

pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    #[must_use]
    pub fn new(directory: &Path, name: &str) -> Self {
        Self {
            path: directory.join(format!("{name}.json")),
        }
    }
    pub fn load(&self) -> Result<Option<Checkpoint>> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("read checkpoint {}", self.path.display()));
            }
        };
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 16 * 1024, "checkpoint exceeds 16 KiB");
        let checkpoint: Checkpoint = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "invalid checkpoint {}; preserve and investigate, do not silently reset",
                self.path.display()
            )
        })?;
        checkpoint.validate()?;
        Ok(Some(checkpoint))
    }
}

impl Store for FileStore {
    fn save(&mut self, checkpoint: &Checkpoint) -> Result<()> {
        checkpoint.validate()?;
        let temporary = self.path.with_extension("json.tmp");
        // The state-directory lock excludes other writers. A crash may leave this temp file.
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(checkpoint)?)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, &self.path).context("atomic checkpoint replacement")?;
        Ok(())
    }
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both NUL-terminated buffers remain live for this synchronous call.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    fs::File::open(to.parent().context("checkpoint has no parent directory")?)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_restart_replacement_and_corruption() {
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "state-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&directory).unwrap();
        let mut store = FileStore::new(&directory, "fixture");
        assert!(store.load().unwrap().is_none());
        let mut checkpoint = Checkpoint {
            schema_version: 1,
            volume_identity: "synthetic-volume".into(),
            journal_id: 11,
            next_usn: 100,
            backfill_end: 200,
            caught_up: false,
        };
        store.save(&checkpoint).unwrap();
        let restarted = FileStore::new(&directory, "fixture");
        assert_eq!(restarted.load().unwrap(), Some(checkpoint.clone()));
        checkpoint.next_usn = 200;
        store.save(&checkpoint).unwrap();
        assert_eq!(restarted.load().unwrap(), Some(checkpoint.clone()));
        let temporary = directory.join("fixture.json.tmp");
        fs::create_dir(&temporary).unwrap();
        checkpoint.next_usn = 300;
        assert!(store.save(&checkpoint).is_err());
        assert_eq!(restarted.load().unwrap().unwrap().next_usn, 200);
        fs::remove_dir(&temporary).unwrap();
        fs::write(directory.join("fixture.json"), b"not-json").unwrap();
        assert!(restarted.load().is_err());
        fs::remove_file(directory.join("fixture.json")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}

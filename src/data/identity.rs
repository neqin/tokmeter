use super::private_io;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

const UUID_SOURCE: &str = "/proc/sys/kernel/random/uuid";

pub fn instance_id_path(home: &str) -> PathBuf {
    private_io::state_dir(home).join("instance-id")
}

pub fn load_or_create(home: &str) -> io::Result<String> {
    load_or_create_at(&instance_id_path(home), Path::new(UUID_SOURCE))
}

fn load_or_create_at(path: &Path, uuid_source: &Path) -> io::Result<String> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let lock_path = parent.join("instance-id.lock");
    let lock = IdentityLock::acquire(&lock_path)?;

    let result = match fs::read_to_string(path) {
        Ok(value) => parse_uuid(&value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let value = fs::read_to_string(uuid_source)?;
            let id = parse_uuid(&value)?;
            private_io::atomic_write_private(path, id.as_bytes())?;
            Ok(id)
        }
        Err(error) => Err(error),
    };
    drop(lock);
    result
}

fn parse_uuid(value: &str) -> io::Result<String> {
    let value = value.trim();
    let valid = value.len() == 36
        && value.chars().enumerate().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    if valid {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid installation id",
        ))
    }
}

struct IdentityLock(File);

impl IdentityLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
    }
}

impl Drop for IdentityLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    const ID: &str = "123e4567-e89b-12d3-a456-426614174000";

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tokmeter-identity-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn reads_existing_identity() {
        let root = temp_dir("existing");
        let path = root.join("instance-id");
        fs::write(&path, ID.to_ascii_uppercase()).unwrap();
        assert_eq!(load_or_create_at(&path, &root.join("missing")).unwrap(), ID);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn creates_private_identity_once() {
        let root = temp_dir("create");
        let path = root.join("state/instance-id");
        let source = root.join("uuid");
        fs::write(&source, format!("{ID}\n")).unwrap();

        assert_eq!(load_or_create_at(&path, &source).unwrap(), ID);
        fs::write(&source, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        assert_eq!(load_or_create_at(&path, &source).unwrap(), ID);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_invalid_existing_identity() {
        let root = temp_dir("invalid");
        let path = root.join("instance-id");
        fs::write(&path, "invalid").unwrap();
        let error = load_or_create_at(&path, &root.join("missing")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(&path).unwrap(), "invalid");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fails_when_uuid_source_is_unavailable() {
        let root = temp_dir("missing");
        let error = load_or_create_at(&root.join("instance-id"), &root.join("uuid")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_first_use_returns_one_identity() {
        let root = temp_dir("concurrent");
        let path = root.join("state/instance-id");
        let source = root.join("uuid");
        fs::write(&source, ID).unwrap();
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let path = path.clone();
            let source = source.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                load_or_create_at(&path, &source).unwrap()
            }));
        }
        let ids: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(ids.iter().all(|id| id == ID));
        assert_eq!(fs::read_to_string(&path).unwrap(), ID);
        let _ = fs::remove_dir_all(root);
    }
}

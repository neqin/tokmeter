use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub fn config_dir(home: &str) -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", home, ".config")
}

pub fn cache_dir(home: &str) -> PathBuf {
    xdg_dir("XDG_CACHE_HOME", home, ".cache")
}

pub fn state_dir(home: &str) -> PathBuf {
    xdg_dir("XDG_STATE_HOME", home, ".local/state")
}

fn xdg_dir(key: &str, home: &str, fallback: &str) -> PathBuf {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(home).join(fallback))
        .join("tokmeter")
}

pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;

    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy();
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), id));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_dir(name: &str) -> PathBuf {
        static ID: AtomicU64 = AtomicU64::new(0);
        let path = env::temp_dir().join(format!(
            "tokmeter-private-io-{name}-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn with_env(key: &str, value: Option<&Path>, f: impl FnOnce()) {
        let old = env::var_os(key);
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
        f();
        match old {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }

    #[test]
    fn xdg_paths_use_overrides() {
        let _guard = env_lock().lock().unwrap();
        let root = temp_dir("xdg");
        with_env("XDG_CONFIG_HOME", Some(&root.join("config")), || {
            with_env("XDG_CACHE_HOME", Some(&root.join("cache")), || {
                with_env("XDG_STATE_HOME", Some(&root.join("state")), || {
                    assert_eq!(config_dir("/home/test"), root.join("config/tokmeter"));
                    assert_eq!(cache_dir("/home/test"), root.join("cache/tokmeter"));
                    assert_eq!(state_dir("/home/test"), root.join("state/tokmeter"));
                });
            });
        });
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn xdg_paths_fall_back_to_home() {
        let _guard = env_lock().lock().unwrap();
        with_env("XDG_CONFIG_HOME", None, || {
            with_env("XDG_CACHE_HOME", None, || {
                with_env("XDG_STATE_HOME", None, || {
                    assert_eq!(
                        config_dir("/home/test"),
                        Path::new("/home/test/.config/tokmeter")
                    );
                    assert_eq!(
                        cache_dir("/home/test"),
                        Path::new("/home/test/.cache/tokmeter")
                    );
                    assert_eq!(
                        state_dir("/home/test"),
                        Path::new("/home/test/.local/state/tokmeter")
                    );
                });
            });
        });
    }

    #[test]
    fn atomic_write_replaces_with_private_file() {
        let root = temp_dir("write");
        let path = root.join("nested/state.json");
        atomic_write_private(&path, b"old").unwrap();
        atomic_write_private(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let _ = fs::remove_dir_all(root);
    }
}

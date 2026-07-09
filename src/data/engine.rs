//! Load / scan / save entrypoint shared with the GUI and `--dump-json`.
//! Cache path and LOCK_NB flock match tok so both tools share one cursor.

use super::cache::{Cache, CACHE_VERSION};
use super::limits;
use super::scan::Scanner;
use super::timeutil::{local_day, local_offset, now_epoch, ymd_str};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};

/// Env defaults (same knobs as tok).
pub fn env_i64(key: &str, default: i64) -> i64 {
    env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

pub fn cache_path(home: &str) -> PathBuf {
    if let Ok(d) = env::var("HERDR_PLUGIN_STATE_DIR") {
        if !d.is_empty() {
            return Path::new(&d).join("cache.json");
        }
    }
    let base = env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{home}/.cache"));
    Path::new(&base).join("tok").join("cache.json")
}

struct CacheLock(File);

impl CacheLock {
    /// Non-blocking exclusive lock. On contention returns Err so caller can
    /// load read-only without saving.
    fn acquire(state_path: &Path) -> std::io::Result<CacheLock> {
        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_path = state_path.with_extension("json.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(lock_path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(CacheLock(file))
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn reset_cache_files(state_path: &Path) {
    let tmp_path = state_path.with_extension("json.tmp");
    for path in [state_path, tmp_path.as_path()] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => eprintln!("tokmeter: failed to remove {}: {e}", path.display()),
        }
    }
}

fn refresh(
    cache: &mut Cache,
    home: &str,
    scan_days: i64,
    history_days: i64,
    hours_days: i64,
    files_days: i64,
) {
    let now = now_epoch();
    let off = local_offset(now);
    let min_mtime = now - (scan_days + 1) * 86400;
    let ring_min = now - hours_days * 86400;
    let scanner = Scanner::new(home, min_mtime, off, ring_min);
    scanner.update(cache);
    let today = local_day(now, off);
    let agg_cutoff = ymd_str(today - history_days);
    let hours_cutoff = ymd_str(today - hours_days);
    let files_mtime_cutoff = now - (files_days + 1) * 86400;
    cache.prune(&agg_cutoff, &hours_cutoff, files_mtime_cutoff, ring_min);
}

/// Incremental scan + optional limits fetch + save. On lock contention returns
/// a loaded cache without saving (read-only).
#[allow(clippy::too_many_arguments)]
pub fn reload_refresh_save(
    state_path: &Path,
    home: &str,
    scan_days: i64,
    history_days: i64,
    hours_days: i64,
    files_days: i64,
    limits_ttl: i64,
    reset: bool,
) -> Cache {
    let lock = match CacheLock::acquire(state_path) {
        Ok(lock) => Some(lock),
        Err(e) => {
            eprintln!("tokmeter: cache lock unavailable ({e}); read-only load");
            return Cache::load(state_path.to_path_buf());
        }
    };
    if reset {
        reset_cache_files(state_path);
    }
    let mut cache = Cache::load(state_path.to_path_buf());

    let upgrade = (1..CACHE_VERSION).contains(&cache.version);
    if upgrade {
        let now = now_epoch();
        let off = local_offset(now);
        let date_cutoff = ymd_str(local_day(now, off) - hours_days);
        let mtime_cutoff = now - hours_days * 86400;
        eprintln!("tokmeter: one-shot hourly rebuild…");
        cache.reset_recent(&date_cutoff, mtime_cutoff);
    }

    if cache.version == 0 || upgrade {
        refresh(
            &mut cache,
            home,
            history_days,
            history_days,
            hours_days,
            files_days,
        );
    } else {
        refresh(
            &mut cache,
            home,
            scan_days,
            history_days,
            hours_days,
            files_days,
        );
    }

    if limits_ttl > 0 {
        static LAST_CLAUDE_FETCH: AtomicI64 = AtomicI64::new(0);
        static LAST_CODEX_FETCH: AtomicI64 = AtomicI64::new(0);
        let now = now_epoch();
        let checked = cache
            .limits
            .get("claude")
            .map_or(0, |s| s.checked)
            .max(LAST_CLAUDE_FETCH.load(Ordering::Relaxed));
        if now - checked >= limits_ttl {
            LAST_CLAUDE_FETCH.store(now, Ordering::Relaxed);
            cache.touch_limits("claude", now);
            if let Some(snap) = limits::fetch_claude(home, now) {
                cache.set_limits("claude", snap);
            }
        }
        let checked = cache
            .limits
            .get("codex")
            .map_or(0, |s| s.checked)
            .max(LAST_CODEX_FETCH.load(Ordering::Relaxed));
        if now - checked >= limits_ttl {
            LAST_CODEX_FETCH.store(now, Ordering::Relaxed);
            cache.touch_limits("codex", now);
            if let Some(snap) = limits::fetch_codex(home, now) {
                cache.set_limits("codex", snap);
            }
        }
    }
    cache.save();
    drop(lock);
    cache
}

/// Convenience: reload using env defaults.
pub fn reload_default(home: &str, limits_ttl: i64, reset: bool) -> Cache {
    let state = cache_path(home);
    let scan_days = env_i64("TOK_WINDOW_DAYS", 8).max(1);
    let history_days = env_i64("TOK_HISTORY_DAYS", 120).max(scan_days);
    let hours_days = env_i64("TOK_HOURS_DAYS", 8).max(1);
    let files_days = env_i64("TOK_FILES_DAYS", 14).max(scan_days);
    reload_refresh_save(
        &state,
        home,
        scan_days,
        history_days,
        hours_days,
        files_days,
        limits_ttl,
        reset,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn cache_path_under_xdg_cache_home() {
        let _g = env_lock().lock().unwrap();
        // SAFETY: single-threaded under mutex for test env vars
        env::set_var("XDG_CACHE_HOME", "/tmp/tokmeter-test-cache");
        env::remove_var("HERDR_PLUGIN_STATE_DIR");
        let p = cache_path("/home/user");
        assert!(
            p.ends_with("tok/cache.json"),
            "path={p:?}"
        );
        assert!(p.to_string_lossy().contains("tokmeter-test-cache"));
        env::remove_var("XDG_CACHE_HOME");
    }

    #[test]
    fn cache_path_honors_herdr_plugin_state_dir() {
        let _g = env_lock().lock().unwrap();
        env::set_var("HERDR_PLUGIN_STATE_DIR", "/tmp/herdr-plugin-state");
        let p = cache_path("/home/user");
        assert_eq!(p, PathBuf::from("/tmp/herdr-plugin-state/cache.json"));
        env::remove_var("HERDR_PLUGIN_STATE_DIR");
    }

    #[test]
    fn dual_reload_leaves_parseable_cache() {
        let _g = env_lock().lock().unwrap();
        let dir = tempfile_dir();
        let state = dir.join("cache.json");
        // Seed a minimal dirty-able cache so save() has something to write.
        fs::write(
            &state,
            r#"{"version":3,"files":{},"agg":{},"hours":{},"rounds":[],"limits":{}}"#,
        )
        .unwrap();
        env::set_var("HERDR_PLUGIN_STATE_DIR", &dir);
        let home = dir.to_string_lossy().to_string();
        let _c1 = reload_refresh_save(&state, &home, 1, 1, 1, 1, 0, false);
        let _c2 = reload_refresh_save(&state, &home, 1, 1, 1, 1, 0, false);
        let text = fs::read_to_string(&state).expect("cache written");
        assert!(text.len() > 2, "cache not truncated");
        let v: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert!(v.is_object());
        assert_eq!(v.get("version").and_then(|x| x.as_u64()), Some(3));
        env::remove_var("HERDR_PLUGIN_STATE_DIR");
        let _ = fs::remove_dir_all(&dir);
    }

    fn tempfile_dir() -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!(
            "tokmeter-engine-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }
}

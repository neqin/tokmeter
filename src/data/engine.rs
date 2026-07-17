//! Load / scan / save entrypoint shared with the GUI and `--dump-json`.
//! Cache path and LOCK_NB flock match tok so both tools share one cursor.

use super::cache::{Cache, CACHE_VERSION};
use super::config::{Config, LimitsConfig, LocalSourcesConfig, RetentionConfig};
use super::limits;
use super::protocol::ExportRefreshStatus;
use super::scan::Scanner;
use super::timeutil::{local_day, local_offset, now_epoch, ymd_str};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};

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

#[derive(Clone, Copy)]
pub struct LocalRefreshOptions {
    pub local_sources: LocalSourcesConfig,
    pub limits: LimitsConfig,
    pub retention: RetentionConfig,
    pub limits_ttl_secs: i64,
    pub reset: bool,
}

pub struct LocalRefreshOutcome {
    pub cache: Cache,
    pub refresh_status: ExportRefreshStatus,
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
            .truncate(false)
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
    local_sources: LocalSourcesConfig,
) {
    let now = now_epoch();
    let off = local_offset(now);
    let min_mtime = now - (scan_days + 1) * 86400;
    let ring_min = now - hours_days * 86400;
    let scanner = Scanner::new_with_sources(home, min_mtime, off, ring_min, local_sources);
    scanner.update(cache);
    let today = local_day(now, off);
    let agg_cutoff = ymd_str(today - history_days);
    let hours_cutoff = ymd_str(today - hours_days);
    let files_mtime_cutoff = now - (files_days + 1) * 86400;
    cache.prune(&agg_cutoff, &hours_cutoff, files_mtime_cutoff, ring_min);
}

#[cfg(test)]
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
    reload_refresh_save_options(
        state_path,
        home,
        LocalRefreshOptions {
            local_sources: LocalSourcesConfig::default(),
            limits: LimitsConfig::default(),
            retention: RetentionConfig {
                scan_days,
                history_days,
                hours_days,
                files_days,
            },
            limits_ttl_secs: limits_ttl,
            reset,
        },
    )
    .cache
}

pub fn reload_configured(
    home: &str,
    config: &Config,
    limits_ttl_secs: i64,
    reset: bool,
) -> LocalRefreshOutcome {
    reload_refresh_save_options(
        &cache_path(home),
        home,
        LocalRefreshOptions {
            local_sources: config.local_sources,
            limits: config.limits,
            retention: config.retention,
            limits_ttl_secs,
            reset,
        },
    )
}

fn reload_refresh_save_options(
    state_path: &Path,
    home: &str,
    options: LocalRefreshOptions,
) -> LocalRefreshOutcome {
    let lock = match CacheLock::acquire(state_path) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("tokmeter: cache lock unavailable ({error}); read-only load");
            return LocalRefreshOutcome {
                cache: Cache::load(state_path.to_path_buf()),
                refresh_status: ExportRefreshStatus::ReadOnly,
            };
        }
    };
    if options.reset {
        reset_cache_files(state_path);
    }
    let mut cache = Cache::load(state_path.to_path_buf());
    let retention = options.retention;

    let upgrade = (1..CACHE_VERSION).contains(&cache.version);
    if upgrade {
        let now = now_epoch();
        let off = local_offset(now);
        let date_cutoff = ymd_str(local_day(now, off) - retention.history_days);
        let mtime_cutoff = now - (retention.history_days + 1) * 86400;
        eprintln!("tokmeter: one-shot history rebuild…");
        cache.reset_recent(&date_cutoff, mtime_cutoff);
    }

    let scan_days = if cache.version == 0 || upgrade {
        retention.history_days
    } else {
        retention.scan_days
    };
    refresh(
        &mut cache,
        home,
        scan_days,
        retention.history_days,
        retention.hours_days,
        retention.files_days,
        options.local_sources,
    );
    refresh_limits(&mut cache, home, options.limits_ttl_secs, options.limits);
    cache.save();
    drop(lock);
    LocalRefreshOutcome {
        cache,
        refresh_status: ExportRefreshStatus::Fresh,
    }
}

fn refresh_limits(cache: &mut Cache, home: &str, limits_ttl: i64, enabled: LimitsConfig) {
    if limits_ttl <= 0 {
        return;
    }
    static LAST_CLAUDE_FETCH: AtomicI64 = AtomicI64::new(0);
    static LAST_CODEX_FETCH: AtomicI64 = AtomicI64::new(0);
    static LAST_GROK_FETCH: AtomicI64 = AtomicI64::new(0);
    let now = now_epoch();
    if enabled.claude {
        let checked = cache
            .limits
            .get("claude")
            .map_or(0, |snapshot| snapshot.checked)
            .max(LAST_CLAUDE_FETCH.load(Ordering::Relaxed));
        if now - checked >= limits_ttl {
            LAST_CLAUDE_FETCH.store(now, Ordering::Relaxed);
            cache.touch_limits("claude", now);
            if let Some(snapshot) = limits::fetch_claude(home, now) {
                cache.set_limits("claude", snapshot);
            }
        }
    }
    if enabled.codex {
        let checked = cache
            .limits
            .get("codex")
            .map_or(0, |snapshot| snapshot.checked)
            .max(LAST_CODEX_FETCH.load(Ordering::Relaxed));
        if now - checked >= limits_ttl {
            LAST_CODEX_FETCH.store(now, Ordering::Relaxed);
            cache.touch_limits("codex", now);
            if let Some(snapshot) = limits::fetch_codex(home, now) {
                cache.set_limits("codex", snapshot);
            }
        }
    }
    if enabled.grok {
        let checked = cache
            .limits
            .get("grok")
            .map_or(0, |snapshot| snapshot.checked)
            .max(LAST_GROK_FETCH.load(Ordering::Relaxed));
        if now - checked >= limits_ttl {
            LAST_GROK_FETCH.store(now, Ordering::Relaxed);
            cache.touch_limits("grok", now);
            let previous = cache.limits.get("grok").cloned();
            if let Some(snapshot) = limits::fetch_grok(home, now, previous.as_ref()) {
                cache.set_limits("grok", snapshot);
            }
        }
    }
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
        assert!(p.ends_with("tok/cache.json"), "path={p:?}");
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
        assert_eq!(
            v.get("version").and_then(|x| x.as_u64()),
            Some(CACHE_VERSION)
        );
        env::remove_var("HERDR_PLUGIN_STATE_DIR");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_contention_returns_read_only() {
        let dir = tempfile_dir();
        let state = dir.join("cache.json");
        let lock = CacheLock::acquire(&state).unwrap();
        let outcome = reload_refresh_save_options(
            &state,
            dir.to_str().unwrap(),
            LocalRefreshOptions {
                local_sources: LocalSourcesConfig::default(),
                limits: LimitsConfig::default(),
                retention: RetentionConfig::default(),
                limits_ttl_secs: 0,
                reset: false,
            },
        );
        assert_eq!(outcome.refresh_status, ExportRefreshStatus::ReadOnly);
        drop(lock);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn disabled_limit_providers_are_not_touched() {
        let dir = tempfile_dir();
        let state = dir.join("cache.json");
        let outcome = reload_refresh_save_options(
            &state,
            dir.to_str().unwrap(),
            LocalRefreshOptions {
                local_sources: LocalSourcesConfig {
                    claude: false,
                    codex: false,
                    omp: false,
                },
                limits: LimitsConfig {
                    claude: false,
                    codex: false,
                    grok: false,
                },
                retention: RetentionConfig::default(),
                limits_ttl_secs: 5,
                reset: false,
            },
        );
        assert!(outcome.cache.limits.is_empty());
        let _ = fs::remove_dir_all(dir);
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

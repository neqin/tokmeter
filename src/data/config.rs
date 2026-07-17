use super::private_io;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

pub const CONFIG_VERSION: u64 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub version: u64,
    pub local_sources: LocalSourcesConfig,
    pub limits: LimitsConfig,
    pub refresh: RefreshConfig,
    pub retention: RetentionConfig,
    pub ssh_sources: Vec<SshSourceConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            local_sources: LocalSourcesConfig::default(),
            limits: LimitsConfig::default(),
            refresh: RefreshConfig::default(),
            retention: RetentionConfig::default(),
            ssh_sources: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LocalSourcesConfig {
    pub claude: bool,
    pub codex: bool,
    pub omp: bool,
}

impl Default for LocalSourcesConfig {
    fn default() -> Self {
        Self {
            claude: true,
            codex: true,
            omp: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LimitsConfig {
    pub claude: bool,
    pub codex: bool,
    pub grok: bool,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            claude: true,
            codex: true,
            grok: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RefreshConfig {
    pub ui_secs: i64,
    pub limits_ttl_secs: i64,
    pub remote_secs: i64,
    pub ssh_connect_timeout_secs: i64,
    pub ssh_command_timeout_secs: i64,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            ui_secs: 3,
            limits_ttl_secs: 300,
            remote_secs: 60,
            ssh_connect_timeout_secs: 5,
            ssh_command_timeout_secs: 30,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RetentionConfig {
    pub scan_days: i64,
    pub history_days: i64,
    pub hours_days: i64,
    pub files_days: i64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            scan_days: 8,
            history_days: 120,
            hours_days: 8,
            files_days: 14,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SshSourceConfig {
    pub id: String,
    pub label: String,
    pub host: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_binary")]
    pub binary: String,
}

pub struct ConfigLoad {
    pub config: Config,
    pub error: Option<String>,
    pub path: PathBuf,
}

pub fn config_path(home: &str) -> PathBuf {
    private_io::config_dir(home).join("config.json")
}

pub fn load(home: &str) -> ConfigLoad {
    load_path(config_path(home))
}

fn load_path(path: PathBuf) -> ConfigLoad {
    let loaded = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Config>(&text)
            .map_err(|error| format!("invalid config: {error}"))
            .and_then(|config| {
                validate(config).map_err(|error| format!("invalid config: {error}"))
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(format!("cannot read config: {error}")),
    };

    let (mut config, error) = match loaded {
        Ok(config) => (config, None),
        Err(error) => (Config::default(), Some(short_error(error))),
    };
    apply_env(&mut config);
    if error.is_some() {
        config.ssh_sources.clear();
    }

    ConfigLoad {
        config,
        error,
        path,
    }
}

fn validate(config: Config) -> Result<Config, String> {
    if config.version != CONFIG_VERSION {
        return Err(format!("unsupported version {}", config.version));
    }
    validate_range("refresh.ui_secs", config.refresh.ui_secs, 1, 3600)?;
    validate_range(
        "refresh.limits_ttl_secs",
        config.refresh.limits_ttl_secs,
        5,
        86400,
    )?;
    validate_range("refresh.remote_secs", config.refresh.remote_secs, 5, 86400)?;
    validate_range(
        "refresh.ssh_connect_timeout_secs",
        config.refresh.ssh_connect_timeout_secs,
        1,
        300,
    )?;
    validate_range(
        "refresh.ssh_command_timeout_secs",
        config.refresh.ssh_command_timeout_secs,
        1,
        3600,
    )?;
    validate_range("retention.scan_days", config.retention.scan_days, 1, 3650)?;
    validate_range(
        "retention.history_days",
        config.retention.history_days,
        config.retention.scan_days,
        3650,
    )?;
    validate_range("retention.hours_days", config.retention.hours_days, 1, 3650)?;
    validate_range(
        "retention.files_days",
        config.retention.files_days,
        config.retention.scan_days,
        3650,
    )?;

    let mut ids = HashSet::new();
    for source in &config.ssh_sources {
        validate_source(source)?;
        if !ids.insert(source.id.as_str()) {
            return Err(format!("duplicate source id {}", source.id));
        }
    }
    Ok(config)
}

fn validate_source(source: &SshSourceConfig) -> Result<(), String> {
    if matches!(source.id.as_str(), "all" | "local") || !valid_id(&source.id) {
        return Err(format!("invalid source id {}", source.id));
    }
    let label_len = source.label.chars().count();
    if label_len == 0 || label_len > 64 || source.label.chars().any(char::is_control) {
        return Err(format!("invalid source label {}", source.id));
    }
    if source.host.is_empty()
        || source.host.len() > 255
        || source.host.starts_with('-')
        || source
            .host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("invalid source host {}", source.id));
    }
    if source.binary.is_empty()
        || source.binary.len() > 4096
        || !source
            .binary
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+'))
    {
        return Err(format!("invalid source binary {}", source.id));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

fn validate_range(name: &str, value: i64, min: i64, max: i64) -> Result<(), String> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be between {min} and {max}"))
    }
}

fn apply_env(config: &mut Config) {
    config.refresh.ui_secs = env_i64("TOK_REFRESH_SECS", config.refresh.ui_secs, 1, 3600);
    config.refresh.limits_ttl_secs = env_i64(
        "TOK_LIMITS_TTL_SECS",
        config.refresh.limits_ttl_secs,
        5,
        86400,
    );
    config.retention.scan_days = env_i64("TOK_WINDOW_DAYS", config.retention.scan_days, 1, 3650);
    config.retention.history_days =
        env_i64("TOK_HISTORY_DAYS", config.retention.history_days, 1, 3650)
            .max(config.retention.scan_days);
    config.retention.hours_days = env_i64("TOK_HOURS_DAYS", config.retention.hours_days, 1, 3650);
    config.retention.files_days = env_i64("TOK_FILES_DAYS", config.retention.files_days, 1, 3650)
        .max(config.retention.scan_days);
}

fn env_i64(key: &str, fallback: i64, min: i64, max: i64) -> i64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| (min..=max).contains(value))
        .unwrap_or(fallback)
}

fn short_error(error: String) -> String {
    error.chars().take(240).collect()
}

fn default_true() -> bool {
    true
}

fn default_binary() -> String {
    "tokmeter".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_dir(name: &str) -> PathBuf {
        static ID: AtomicU64 = AtomicU64::new(0);
        let path = env::temp_dir().join(format!(
            "tokmeter-config-{name}-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn without_env(f: impl FnOnce()) {
        let keys = [
            "TOK_REFRESH_SECS",
            "TOK_LIMITS_TTL_SECS",
            "TOK_WINDOW_DAYS",
            "TOK_HISTORY_DAYS",
            "TOK_HOURS_DAYS",
            "TOK_FILES_DAYS",
        ];
        let old: Vec<_> = keys.iter().map(env::var_os).collect();
        for key in keys {
            env::remove_var(key);
        }
        f();
        for (key, value) in keys.into_iter().zip(old) {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }

    fn write_config(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn no_file_uses_defaults() {
        let _guard = env_lock().lock().unwrap();
        without_env(|| {
            let root = temp_dir("defaults");
            let load = load_path(root.join("config.json"));
            assert_eq!(load.config, Config::default());
            assert!(load.error.is_none());
            let _ = fs::remove_dir_all(root);
        });
    }

    #[test]
    fn partial_config_uses_nested_defaults_and_unknown_fields() {
        let _guard = env_lock().lock().unwrap();
        without_env(|| {
            let root = temp_dir("partial");
            let path = root.join("config.json");
            write_config(
                &path,
                r#"{
                    "version": 1,
                    "local_sources": {"omp": false},
                    "refresh": {"remote_secs": 90},
                    "unknown": true,
                    "ssh_sources": [{"id":"lxc","label":"LXC","host":"user@lxc"}]
                }"#,
            );
            let load = load_path(path);
            assert!(load.error.is_none());
            assert!(load.config.local_sources.claude);
            assert!(!load.config.local_sources.omp);
            assert_eq!(load.config.refresh.remote_secs, 90);
            assert_eq!(load.config.ssh_sources[0].binary, "tokmeter");
            assert!(load.config.ssh_sources[0].enabled);
            let _ = fs::remove_dir_all(root);
        });
    }

    #[test]
    fn env_overrides_file_and_invalid_env_keeps_file_value() {
        let _guard = env_lock().lock().unwrap();
        without_env(|| {
            let root = temp_dir("env");
            let path = root.join("config.json");
            write_config(
                &path,
                r#"{"refresh":{"ui_secs":9},"retention":{"scan_days":10,"history_days":20,"files_days":12}}"#,
            );
            env::set_var("TOK_REFRESH_SECS", "7");
            env::set_var("TOK_WINDOW_DAYS", "invalid");
            env::set_var("TOK_HISTORY_DAYS", "5");
            let load = load_path(path);
            assert_eq!(load.config.refresh.ui_secs, 7);
            assert_eq!(load.config.retention.scan_days, 10);
            assert_eq!(load.config.retention.history_days, 10);
            env::remove_var("TOK_REFRESH_SECS");
            env::remove_var("TOK_WINDOW_DAYS");
            env::remove_var("TOK_HISTORY_DAYS");
            let _ = fs::remove_dir_all(root);
        });
    }

    #[test]
    fn invalid_config_falls_back_to_local_only_defaults() {
        let _guard = env_lock().lock().unwrap();
        without_env(|| {
            for (name, body) in [
                ("malformed", "{"),
                ("version", r#"{"version":2}"#),
                (
                    "duplicate",
                    r#"{"ssh_sources":[{"id":"a","label":"A","host":"a"},{"id":"a","label":"B","host":"b"}]}"#,
                ),
                (
                    "reserved",
                    r#"{"ssh_sources":[{"id":"local","label":"A","host":"a"}]}"#,
                ),
                (
                    "host",
                    r#"{"ssh_sources":[{"id":"a","label":"A","host":"-bad"}]}"#,
                ),
                (
                    "binary",
                    r#"{"ssh_sources":[{"id":"a","label":"A","host":"a","binary":"tokmeter;rm"}]}"#,
                ),
            ] {
                let root = temp_dir(name);
                let path = root.join("config.json");
                write_config(&path, body);
                let load = load_path(path);
                assert!(load.error.is_some(), "{name}");
                assert!(load.config.ssh_sources.is_empty(), "{name}");
                assert_eq!(load.config.local_sources, LocalSourcesConfig::default());
                let _ = fs::remove_dir_all(root);
            }
        });
    }

    #[test]
    fn disabled_source_is_retained() {
        let _guard = env_lock().lock().unwrap();
        without_env(|| {
            let root = temp_dir("disabled");
            let path = root.join("config.json");
            write_config(
                &path,
                r#"{"ssh_sources":[{"id":"lxc","label":"LXC","host":"lxc","enabled":false}]}"#,
            );
            let load = load_path(path);
            assert!(load.error.is_none());
            assert!(!load.config.ssh_sources[0].enabled);
            let _ = fs::remove_dir_all(root);
        });
    }
}

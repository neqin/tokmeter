use super::private_io;
use super::protocol::{decode_json, ProtocolRetention, SourceSnapshot, SourceWarning};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

pub const REMOTE_STORE_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceHealth {
    Disabled,
    Connecting,
    Healthy,
    Stale,
    Error,
    Incompatible,
    DuplicateInstance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptFailureKind {
    Error,
    Incompatible,
}

#[derive(Clone)]
pub struct StoredSource {
    pub label: String,
    pub instance_id: String,
    pub snapshot: Option<SourceSnapshot>,
    pub last_attempt: i64,
    pub last_success: i64,
    pub duration_ms: u64,
    pub health: SourceHealth,
    pub warnings: Vec<SourceWarning>,
    pub error: String,
}

pub struct RemoteStore {
    pub sources: HashMap<String, StoredSource>,
    pub load_error: Option<String>,
    path: PathBuf,
    writable: bool,
    dirty: bool,
}

pub fn remote_store_path(home: &str) -> PathBuf {
    private_io::cache_dir(home).join("remote.json")
}

impl RemoteStore {
    pub fn load(home: &str, now: i64, retention: ProtocolRetention) -> Self {
        Self::load_path(remote_store_path(home), now, retention)
    }

    #[cfg(test)]
    pub fn empty(home: &str) -> Self {
        Self {
            sources: HashMap::new(),
            load_error: None,
            path: remote_store_path(home),
            writable: true,
            dirty: false,
        }
    }

    fn load_path(path: PathBuf, now: i64, retention: ProtocolRetention) -> Self {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Self {
                    sources: HashMap::new(),
                    load_error: None,
                    path,
                    writable: true,
                    dirty: false,
                };
            }
            Err(error) => {
                return Self::failed(path, format!("cannot read remote cache: {error}"));
            }
        };
        match decode_store(&text, now, retention) {
            Ok(sources) => Self {
                sources,
                load_error: None,
                path,
                writable: true,
                dirty: false,
            },
            Err(error) => Self::failed(path, error),
        }
    }

    fn failed(path: PathBuf, error: String) -> Self {
        Self {
            sources: HashMap::new(),
            load_error: Some(short_error(error)),
            path,
            writable: false,
            dirty: false,
        }
    }

    pub fn set_connecting(&mut self, source_id: &str, label: &str, attempt: i64) {
        let entry = self.entry_mut(source_id, label);
        entry.last_attempt = attempt;
        entry.health = SourceHealth::Connecting;
        entry.error.clear();
    }

    pub fn apply_success(
        &mut self,
        source_id: &str,
        label: &str,
        snapshot: SourceSnapshot,
        warnings: Vec<SourceWarning>,
        attempt: i64,
        duration_ms: u64,
    ) {
        let entry = self.entry_mut(source_id, label);
        entry.label = label.to_string();
        entry.instance_id = snapshot.instance_id.clone();
        entry.snapshot = Some(snapshot);
        entry.last_attempt = attempt;
        entry.last_success = attempt;
        entry.duration_ms = duration_ms;
        entry.health = SourceHealth::Healthy;
        entry.warnings = warnings;
        entry.error.clear();
        self.dirty = true;
    }

    pub fn apply_failure(
        &mut self,
        source_id: &str,
        label: &str,
        kind: AttemptFailureKind,
        error: impl Into<String>,
        attempt: i64,
        duration_ms: u64,
    ) {
        let entry = self.entry_mut(source_id, label);
        entry.label = label.to_string();
        entry.last_attempt = attempt;
        entry.duration_ms = duration_ms;
        entry.health = match kind {
            AttemptFailureKind::Incompatible => SourceHealth::Incompatible,
            AttemptFailureKind::Error if entry.snapshot.is_some() => SourceHealth::Stale,
            AttemptFailureKind::Error => SourceHealth::Error,
        };
        entry.error = short_error(error.into());
        self.dirty = true;
    }

    pub fn save(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "remote cache is invalid; remove it before saving",
            ));
        }
        let text = serde_json::to_vec(&self.to_value())?;
        private_io::atomic_write_private(&self.path, &text)?;
        self.dirty = false;
        Ok(())
    }

    fn entry_mut(&mut self, source_id: &str, label: &str) -> &mut StoredSource {
        self.sources
            .entry(source_id.to_string())
            .or_insert_with(|| StoredSource {
                label: label.to_string(),
                instance_id: String::new(),
                snapshot: None,
                last_attempt: 0,
                last_success: 0,
                duration_ms: 0,
                health: SourceHealth::Error,
                warnings: Vec::new(),
                error: String::new(),
            })
    }

    fn to_value(&self) -> Value {
        let sources = self
            .sources
            .iter()
            .map(|(source_id, source)| {
                let mut value = Map::new();
                value.insert("label".into(), source.label.clone().into());
                value.insert("instance_id".into(), source.instance_id.clone().into());
                if let Some(snapshot) = &source.snapshot {
                    value.insert("snapshot".into(), snapshot.to_value());
                }
                value.insert("last_attempt".into(), source.last_attempt.into());
                value.insert("last_success".into(), source.last_success.into());
                value.insert("duration_ms".into(), source.duration_ms.into());
                value.insert("health".into(), persisted_health(source).into());
                value.insert(
                    "warnings".into(),
                    Value::Array(
                        source
                            .warnings
                            .iter()
                            .map(|warning| warning_name(*warning).into())
                            .collect(),
                    ),
                );
                value.insert("error".into(), source.error.clone().into());
                (source_id.clone(), Value::Object(value))
            })
            .collect();
        let mut root = Map::new();
        root.insert("version".into(), REMOTE_STORE_VERSION.into());
        root.insert("sources".into(), Value::Object(sources));
        Value::Object(root)
    }
}

fn decode_store(
    text: &str,
    now: i64,
    retention: ProtocolRetention,
) -> Result<HashMap<String, StoredSource>, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let root = value
        .as_object()
        .ok_or_else(|| "remote cache must be an object".to_string())?;
    if root.get("version").and_then(Value::as_u64) != Some(REMOTE_STORE_VERSION) {
        return Err("unsupported remote cache version".to_string());
    }
    let values = root
        .get("sources")
        .and_then(Value::as_object)
        .ok_or_else(|| "missing remote cache sources".to_string())?;
    let mut sources = HashMap::new();
    for (source_id, value) in values {
        if source_id.is_empty() || source_id.len() > 64 {
            return Err("invalid remote cache source id".to_string());
        }
        let value = value
            .as_object()
            .ok_or_else(|| format!("invalid remote cache source {source_id}"))?;
        let label = value
            .get("label")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing label for {source_id}"))?
            .to_string();
        let snapshot = match value.get("snapshot") {
            Some(snapshot) => {
                let text = serde_json::to_string(snapshot).map_err(|error| error.to_string())?;
                Some(
                    decode_json(&text, now, retention)
                        .map_err(|error| error.to_string())?
                        .snapshot,
                )
            }
            None => None,
        };
        let instance_id = value
            .get("instance_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.instance_id != instance_id)
        {
            return Err(format!("instance id mismatch for {source_id}"));
        }
        let last_attempt = nonnegative_i64(value, "last_attempt", source_id)?;
        let last_success = nonnegative_i64(value, "last_success", source_id)?;
        let duration_ms = value
            .get("duration_ms")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("missing duration for {source_id}"))?;
        let health = parse_health(
            value
                .get("health")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("missing health for {source_id}"))?,
        )?;
        let warnings = value
            .get("warnings")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("missing warnings for {source_id}"))?
            .iter()
            .map(|warning| {
                warning
                    .as_str()
                    .ok_or_else(|| format!("invalid warning for {source_id}"))
                    .and_then(parse_warning)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        sources.insert(
            source_id.clone(),
            StoredSource {
                label,
                instance_id,
                snapshot,
                last_attempt,
                last_success,
                duration_ms,
                health,
                warnings,
                error: short_error(error),
            },
        );
    }
    Ok(sources)
}

fn nonnegative_i64(value: &Map<String, Value>, key: &str, source_id: &str) -> Result<i64, String> {
    let value = value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing {key} for {source_id}"))?;
    if value < 0 {
        Err(format!("invalid {key} for {source_id}"))
    } else {
        Ok(value)
    }
}

fn persisted_health(source: &StoredSource) -> &'static str {
    match source.health {
        SourceHealth::Disabled => "disabled",
        SourceHealth::Connecting if source.snapshot.is_some() => "stale",
        SourceHealth::Connecting => "error",
        SourceHealth::Healthy => "healthy",
        SourceHealth::Stale => "stale",
        SourceHealth::Error => "error",
        SourceHealth::Incompatible => "incompatible",
        SourceHealth::DuplicateInstance => "duplicate_instance",
    }
}

fn parse_health(value: &str) -> Result<SourceHealth, String> {
    match value {
        "disabled" => Ok(SourceHealth::Disabled),
        "healthy" => Ok(SourceHealth::Healthy),
        "stale" => Ok(SourceHealth::Stale),
        "error" => Ok(SourceHealth::Error),
        "incompatible" => Ok(SourceHealth::Incompatible),
        "duplicate_instance" => Ok(SourceHealth::DuplicateInstance),
        _ => Err("invalid source health".to_string()),
    }
}

fn warning_name(warning: SourceWarning) -> &'static str {
    match warning {
        SourceWarning::PartialHistory => "partial_history",
        SourceWarning::ReadOnlyRefresh => "read_only_refresh",
    }
}

fn parse_warning(value: &str) -> Result<SourceWarning, String> {
    match value {
        "partial_history" => Ok(SourceWarning::PartialHistory),
        "read_only_refresh" => Ok(SourceWarning::ReadOnlyRefresh),
        _ => Err("invalid source warning".to_string()),
    }
}

fn short_error(error: impl Into<String>) -> String {
    error
        .into()
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::cache::{CompactData, CACHE_VERSION};
    use crate::data::protocol::{ExportRefreshStatus, ProtocolRetention};
    use crate::data::timeutil::local_offset;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    const NOW: i64 = 1_784_317_200;
    const ID1: &str = "123e4567-e89b-12d3-a456-426614174000";
    const ID2: &str = "223e4567-e89b-12d3-a456-426614174000";

    fn retention() -> ProtocolRetention {
        ProtocolRetention {
            history_days: 120,
            hours_days: 8,
        }
    }

    fn snapshot(id: &str) -> SourceSnapshot {
        SourceSnapshot {
            app_version: "0.1.8".into(),
            cache_version: CACHE_VERSION,
            instance_id: id.into(),
            generated_at: NOW,
            utc_offset_secs: local_offset(NOW),
            refresh_status: ExportRefreshStatus::Fresh,
            retention: retention(),
            data: CompactData::default(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "tokmeter-remote-store-{name}-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn store(path: PathBuf) -> RemoteStore {
        RemoteStore {
            sources: HashMap::new(),
            load_error: None,
            path,
            writable: true,
            dirty: false,
        }
    }

    #[test]
    fn success_replaces_snapshot_and_instance() {
        let path = temp_path("replace");
        let mut store = store(path);
        store.apply_success("lxc", "LXC", snapshot(ID1), Vec::new(), NOW, 10);
        store.apply_success("lxc", "LXC", snapshot(ID2), Vec::new(), NOW + 1, 20);
        let source = &store.sources["lxc"];
        assert_eq!(source.instance_id, ID2);
        assert_eq!(source.snapshot.as_ref().unwrap().instance_id, ID2);
        assert_eq!(source.last_success, NOW + 1);
    }

    #[test]
    fn failures_retain_last_good_snapshot() {
        let path = temp_path("failure");
        let mut store = store(path);
        store.apply_success("lxc", "LXC", snapshot(ID1), Vec::new(), NOW, 10);
        store.apply_failure(
            "lxc",
            "LXC",
            AttemptFailureKind::Error,
            "offline",
            NOW + 1,
            20,
        );
        assert_eq!(store.sources["lxc"].health, SourceHealth::Stale);
        assert_eq!(
            store.sources["lxc"].snapshot.as_ref().unwrap().instance_id,
            ID1
        );
        store.apply_failure(
            "lxc",
            "LXC",
            AttemptFailureKind::Incompatible,
            "version",
            NOW + 2,
            30,
        );
        assert_eq!(store.sources["lxc"].health, SourceHealth::Incompatible);
        assert!(store.sources["lxc"].snapshot.is_some());
    }

    #[test]
    fn saves_and_loads_private_store() {
        let path = temp_path("roundtrip");
        let mut store = store(path.clone());
        store.apply_success(
            "lxc",
            "LXC",
            snapshot(ID1),
            vec![SourceWarning::ReadOnlyRefresh],
            NOW,
            10,
        );
        store.save().unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = RemoteStore::load_path(path.clone(), NOW, retention());
        assert!(loaded.load_error.is_none());
        assert_eq!(loaded.sources["lxc"].instance_id, ID1);
        assert_eq!(
            loaded.sources["lxc"].warnings,
            vec![SourceWarning::ReadOnlyRefresh]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn corrupt_store_is_not_overwritten() {
        let path = temp_path("corrupt");
        fs::write(&path, "{").unwrap();
        let mut loaded = RemoteStore::load_path(path.clone(), NOW, retention());
        assert!(loaded.load_error.is_some());
        loaded.apply_failure("lxc", "LXC", AttemptFailureKind::Error, "offline", NOW, 1);
        assert_eq!(
            loaded.save().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn connecting_is_not_persisted() {
        let path = temp_path("connecting");
        let mut store = store(path.clone());
        store.apply_success("lxc", "LXC", snapshot(ID1), Vec::new(), NOW, 10);
        store.set_connecting("lxc", "LXC", NOW + 1);
        store.dirty = true;
        store.save().unwrap();
        let loaded = RemoteStore::load_path(path.clone(), NOW + 1, retention());
        assert_eq!(loaded.sources["lxc"].health, SourceHealth::Stale);
        let _ = fs::remove_file(path);
    }
}

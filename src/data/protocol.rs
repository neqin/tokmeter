use super::cache::{compact_from_value, compact_to_value, CompactData, CACHE_VERSION};
use super::timeutil::local_offset;
use serde_json::{Map, Value};
use std::fmt;

pub const PROTOCOL_NAME: &str = "tokmeter-source-snapshot";
pub const PROTOCOL_VERSION: u64 = 1;
pub const MAX_STDOUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FUTURE_SECS: i64 = 300;
const MAX_UTC_OFFSET_SECS: i64 = 18 * 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportRefreshStatus {
    Fresh,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolRetention {
    pub history_days: i64,
    pub hours_days: i64,
}

#[derive(Clone)]
pub struct SourceSnapshot {
    pub app_version: String,
    pub cache_version: u64,
    pub instance_id: String,
    pub generated_at: i64,
    pub utc_offset_secs: i64,
    pub refresh_status: ExportRefreshStatus,
    pub retention: ProtocolRetention,
    pub data: CompactData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceWarning {
    PartialHistory,
    ReadOnlyRefresh,
}

pub struct DecodedSnapshot {
    pub snapshot: SourceSnapshot,
    pub warnings: Vec<SourceWarning>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolErrorKind {
    Invalid,
    Incompatible,
}

#[derive(Debug)]
pub struct ProtocolError {
    pub kind: ProtocolErrorKind,
    pub message: String,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl SourceSnapshot {
    pub fn to_value(&self) -> Value {
        let mut retention = Map::new();
        retention.insert("history_days".into(), self.retention.history_days.into());
        retention.insert("hours_days".into(), self.retention.hours_days.into());

        let mut root = Map::new();
        root.insert("protocol".into(), PROTOCOL_NAME.into());
        root.insert("protocol_version".into(), PROTOCOL_VERSION.into());
        root.insert("app_version".into(), self.app_version.clone().into());
        root.insert("cache_version".into(), self.cache_version.into());
        root.insert("instance_id".into(), self.instance_id.clone().into());
        root.insert("generated_at".into(), self.generated_at.into());
        root.insert("utc_offset_secs".into(), self.utc_offset_secs.into());
        root.insert(
            "refresh_status".into(),
            match self.refresh_status {
                ExportRefreshStatus::Fresh => "fresh",
                ExportRefreshStatus::ReadOnly => "read_only",
            }
            .into(),
        );
        root.insert("retention".into(), Value::Object(retention));
        root.insert("data".into(), compact_to_value(&self.data));
        Value::Object(root)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.to_value())
    }
}

pub fn decode_json(
    text: &str,
    now: i64,
    local_retention: ProtocolRetention,
) -> Result<DecodedSnapshot, ProtocolError> {
    if text.len() > MAX_STDOUT_BYTES {
        return Err(invalid("snapshot exceeds output limit"));
    }
    let value: Value = serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
    let root = value
        .as_object()
        .ok_or_else(|| invalid("snapshot must be an object"))?;

    if string(root, "protocol")? != PROTOCOL_NAME {
        return Err(invalid("unexpected protocol name"));
    }
    if u64_value(root, "protocol_version")? != PROTOCOL_VERSION {
        return Err(incompatible("unsupported protocol version"));
    }
    let app_version = string(root, "app_version")?.to_string();
    if app_version.is_empty() {
        return Err(invalid("missing app version"));
    }
    let cache_version = u64_value(root, "cache_version")?;
    if cache_version != CACHE_VERSION {
        return Err(incompatible("unsupported cache version"));
    }
    let instance_id = string(root, "instance_id")?.to_ascii_lowercase();
    if !valid_uuid(&instance_id) {
        return Err(invalid("invalid instance id"));
    }
    let generated_at = i64_value(root, "generated_at")?;
    if generated_at <= 0 || generated_at > now.saturating_add(MAX_FUTURE_SECS) {
        return Err(invalid("invalid generated timestamp"));
    }
    let utc_offset_secs = i64_value(root, "utc_offset_secs")?;
    if !(-MAX_UTC_OFFSET_SECS..=MAX_UTC_OFFSET_SECS).contains(&utc_offset_secs) {
        return Err(invalid("invalid utc offset"));
    }
    if utc_offset_secs != local_offset(generated_at) {
        return Err(incompatible("timezone offset mismatch"));
    }
    let refresh_status = match string(root, "refresh_status")? {
        "fresh" => ExportRefreshStatus::Fresh,
        "read_only" => ExportRefreshStatus::ReadOnly,
        _ => return Err(invalid("invalid refresh status")),
    };
    let retention_value = root
        .get("retention")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("missing retention"))?;
    let retention = ProtocolRetention {
        history_days: i64_value(retention_value, "history_days")?,
        hours_days: i64_value(retention_value, "hours_days")?,
    };
    if !(1..=3650).contains(&retention.history_days) || !(1..=3650).contains(&retention.hours_days)
    {
        return Err(invalid("invalid retention"));
    }
    let data = compact_from_value(
        root.get("data")
            .ok_or_else(|| invalid("missing compact data"))?,
    )
    .map_err(invalid)?;

    let mut warnings = Vec::new();
    if retention.history_days < local_retention.history_days
        || retention.hours_days < local_retention.hours_days
    {
        warnings.push(SourceWarning::PartialHistory);
    }
    if refresh_status == ExportRefreshStatus::ReadOnly {
        warnings.push(SourceWarning::ReadOnlyRefresh);
    }

    Ok(DecodedSnapshot {
        snapshot: SourceSnapshot {
            app_version,
            cache_version,
            instance_id,
            generated_at,
            utc_offset_secs,
            refresh_status,
            retention,
            data,
        },
        warnings,
    })
}

fn string<'a>(root: &'a Map<String, Value>, key: &str) -> Result<&'a str, ProtocolError> {
    root.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("missing {key}")))
}

fn u64_value(root: &Map<String, Value>, key: &str) -> Result<u64, ProtocolError> {
    root.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("missing {key}")))
}

fn i64_value(root: &Map<String, Value>, key: &str) -> Result<i64, ProtocolError> {
    root.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid(format!("missing {key}")))
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.chars().enumerate().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError {
        kind: ProtocolErrorKind::Invalid,
        message: message.into(),
    }
}

fn incompatible(message: impl Into<String>) -> ProtocolError {
    ProtocolError {
        kind: ProtocolErrorKind::Incompatible,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::cache::{agg_key, Round, SEP};
    use crate::data::limits::{Snapshot, Window};
    use std::collections::HashMap;

    const ID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const NOW: i64 = 1_784_317_200;

    fn sample_snapshot() -> SourceSnapshot {
        let mut data = CompactData::default();
        data.agg.insert(
            agg_key("2026-07-17", "claude", "opus", "standard", "/proj"),
            [1, 2, 3, 4, 5, 6],
        );
        data.hours
            .insert(format!("2026-07-17 19{SEP}claude{SEP}/proj"), [20, 1]);
        data.rounds.push(Round {
            ts: NOW,
            agent: "claude".into(),
            model: "opus".into(),
            speed: "standard".into(),
            project: "/proj".into(),
            inp: 2,
            cread: 3,
            cw5: 4,
            cw1h: 5,
            out: 6,
        });
        data.limits = HashMap::from([(
            "claude".into(),
            Snapshot {
                ts: NOW,
                checked: NOW,
                windows: vec![Window {
                    label: "5h".into(),
                    pct: 42.0,
                    resets: NOW + 1000,
                }],
            },
        )]);
        SourceSnapshot {
            app_version: "0.1.8".into(),
            cache_version: CACHE_VERSION,
            instance_id: ID.into(),
            generated_at: NOW,
            utc_offset_secs: local_offset(NOW),
            refresh_status: ExportRefreshStatus::Fresh,
            retention: ProtocolRetention {
                history_days: 120,
                hours_days: 8,
            },
            data,
        }
    }

    fn decode(value: Value) -> Result<DecodedSnapshot, ProtocolError> {
        decode_json(
            &serde_json::to_string(&value).unwrap(),
            NOW,
            ProtocolRetention {
                history_days: 120,
                hours_days: 8,
            },
        )
    }

    #[test]
    fn round_trips_protocol_v1() {
        let snapshot = sample_snapshot();
        let decoded = decode_json(&snapshot.to_json().unwrap(), NOW, snapshot.retention).unwrap();
        assert_eq!(decoded.snapshot.instance_id, ID);
        assert_eq!(decoded.snapshot.data.agg, snapshot.data.agg);
        assert_eq!(decoded.snapshot.data.rounds, snapshot.data.rounds);
        assert!(decoded.warnings.is_empty());
    }

    #[test]
    fn accepts_unknown_fields() {
        let mut value = sample_snapshot().to_value();
        value["future"] = true.into();
        assert!(decode(value).is_ok());
    }

    #[test]
    fn rejects_incompatible_versions_and_timezone() {
        for (field, value) in [
            ("protocol_version", PROTOCOL_VERSION + 1),
            ("cache_version", CACHE_VERSION + 1),
        ] {
            let mut snapshot = sample_snapshot().to_value();
            snapshot[field] = value.into();
            let error = decode(snapshot).err().unwrap();
            assert_eq!(error.kind, ProtocolErrorKind::Incompatible);
        }
        let mut snapshot = sample_snapshot().to_value();
        snapshot["utc_offset_secs"] = (local_offset(NOW) + 3600).into();
        let error = decode(snapshot).err().unwrap();
        assert_eq!(error.kind, ProtocolErrorKind::Incompatible);
    }

    #[test]
    fn rejects_invalid_identity_timestamp_and_compact_data() {
        let mut invalid_id = sample_snapshot().to_value();
        invalid_id["instance_id"] = "bad".into();
        assert_eq!(
            decode(invalid_id).err().unwrap().kind,
            ProtocolErrorKind::Invalid
        );

        let mut future = sample_snapshot().to_value();
        future["generated_at"] = (NOW + MAX_FUTURE_SECS + 1).into();
        assert_eq!(
            decode(future).err().unwrap().kind,
            ProtocolErrorKind::Invalid
        );

        let mut malformed = sample_snapshot().to_value();
        malformed["data"]["agg"] = Value::Array(Vec::new());
        assert_eq!(
            decode(malformed).err().unwrap().kind,
            ProtocolErrorKind::Invalid
        );
    }

    #[test]
    fn returns_retention_and_read_only_warnings() {
        let mut snapshot = sample_snapshot();
        snapshot.retention.history_days = 30;
        snapshot.retention.hours_days = 2;
        snapshot.refresh_status = ExportRefreshStatus::ReadOnly;
        let decoded = decode(snapshot.to_value()).unwrap();
        assert_eq!(
            decoded.warnings,
            vec![
                SourceWarning::PartialHistory,
                SourceWarning::ReadOnlyRefresh
            ]
        );
    }

    #[test]
    fn rejects_oversized_json_before_parsing() {
        let text = " ".repeat(MAX_STDOUT_BYTES + 1);
        let error = decode_json(
            &text,
            NOW,
            ProtocolRetention {
                history_days: 120,
                hours_days: 8,
            },
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, ProtocolErrorKind::Invalid);
    }
}

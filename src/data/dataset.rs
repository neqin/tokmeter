use super::cache::{Cache, CompactData, Counts, Round};
use super::config::Config;
use super::limits::Snapshot;
use super::protocol::SourceWarning;
use super::remote_store::{RemoteStore, SourceHealth};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceFilter {
    All,
    Local,
    Remote(String),
}

impl SourceFilter {
    pub fn parse(value: &str) -> Self {
        match value {
            "all" => Self::All,
            "local" => Self::Local,
            value => Self::Remote(value.to_string()),
        }
    }
}

#[derive(Clone)]
pub struct SourceDescriptor {
    pub id: String,
    pub label: String,
    pub instance_id: String,
    pub local: bool,
    pub enabled: bool,
    pub active: bool,
    pub health: SourceHealth,
    pub warnings: Vec<SourceWarning>,
    pub last_attempt: i64,
    pub last_success: i64,
    pub duration_ms: u64,
    pub error: String,
}

enum DataRef<'a> {
    Local(&'a Cache),
    Remote(&'a CompactData),
}

struct DatasetSource<'a> {
    descriptor: SourceDescriptor,
    data: Option<DataRef<'a>>,
}

pub struct DataView<'a> {
    pub source: &'a SourceDescriptor,
    pub agg: &'a HashMap<String, Counts>,
    pub hours: &'a HashMap<String, [u64; 2]>,
    pub rounds: &'a [Round],
    pub limits: &'a HashMap<String, Snapshot>,
}

#[cfg(test)]
pub struct LimitView<'a> {
    pub source: &'a SourceDescriptor,
    pub agent: &'a str,
    pub snapshot: &'a Snapshot,
}

pub struct Dataset<'a> {
    sources: Vec<DatasetSource<'a>>,
}

impl<'a> Dataset<'a> {
    pub fn new(
        local: &'a Cache,
        config: &'a Config,
        local_instance_id: Option<&str>,
        store: &'a RemoteStore,
        now: i64,
    ) -> Self {
        let mut sources = Vec::with_capacity(config.ssh_sources.len() + 1);
        let mut instances = HashSet::new();
        if let Some(instance_id) = local_instance_id.filter(|id| !id.is_empty()) {
            instances.insert(instance_id.to_string());
        }
        sources.push(DatasetSource {
            descriptor: SourceDescriptor {
                id: "local".to_string(),
                label: "local".to_string(),
                instance_id: local_instance_id.unwrap_or("").to_string(),
                local: true,
                enabled: true,
                active: true,
                health: SourceHealth::Healthy,
                warnings: Vec::new(),
                last_attempt: 0,
                last_success: 0,
                duration_ms: 0,
                error: String::new(),
            },
            data: Some(DataRef::Local(local)),
        });

        let stale_after = config.refresh.remote_secs.saturating_mul(3).max(300);
        for configured in &config.ssh_sources {
            let stored = store.sources.get(&configured.id);
            let snapshot = stored.and_then(|source| source.snapshot.as_ref());
            let mut health = stored
                .map(|source| source.health)
                .unwrap_or(SourceHealth::Error);
            if configured.enabled
                && matches!(
                    health,
                    SourceHealth::Disabled | SourceHealth::DuplicateInstance
                )
            {
                health = if snapshot.is_some() {
                    SourceHealth::Healthy
                } else {
                    SourceHealth::Error
                };
            }
            if configured.enabled
                && health == SourceHealth::Healthy
                && stored.is_some_and(|source| {
                    source.last_success > 0 && now.saturating_sub(source.last_success) > stale_after
                })
            {
                health = SourceHealth::Stale;
            }
            if !configured.enabled {
                health = SourceHealth::Disabled;
            }

            let candidate = configured.enabled
                && snapshot.is_some()
                && matches!(
                    health,
                    SourceHealth::Connecting
                        | SourceHealth::Healthy
                        | SourceHealth::Stale
                        | SourceHealth::Incompatible
                );
            let instance_id = snapshot
                .map(|snapshot| snapshot.instance_id.as_str())
                .or_else(|| stored.map(|source| source.instance_id.as_str()))
                .unwrap_or("");
            let duplicate =
                candidate && !instance_id.is_empty() && !instances.insert(instance_id.to_string());
            if duplicate {
                health = SourceHealth::DuplicateInstance;
            }
            let active = candidate && !duplicate;
            let descriptor = SourceDescriptor {
                id: configured.id.clone(),
                label: configured.label.clone(),
                instance_id: instance_id.to_string(),
                local: false,
                enabled: configured.enabled,
                active,
                health,
                warnings: stored
                    .map(|source| source.warnings.clone())
                    .unwrap_or_default(),
                last_attempt: stored.map(|source| source.last_attempt).unwrap_or(0),
                last_success: stored.map(|source| source.last_success).unwrap_or(0),
                duration_ms: stored.map(|source| source.duration_ms).unwrap_or(0),
                error: stored
                    .map(|source| source.error.clone())
                    .unwrap_or_default(),
            };
            sources.push(DatasetSource {
                descriptor,
                data: if active {
                    snapshot.map(|snapshot| DataRef::Remote(&snapshot.data))
                } else {
                    None
                },
            });
        }
        Self { sources }
    }

    pub fn sources(&self) -> impl Iterator<Item = &SourceDescriptor> {
        self.sources.iter().map(|source| &source.descriptor)
    }

    pub fn source(&self, id: &str) -> Option<&SourceDescriptor> {
        self.sources
            .iter()
            .find(|source| source.descriptor.id == id)
            .map(|source| &source.descriptor)
    }

    pub fn selected(&self, filter: &SourceFilter) -> Result<Vec<DataView<'_>>, String> {
        if let SourceFilter::Remote(id) = filter {
            if self.source(id).is_none() {
                return Err(format!("unknown source {id}"));
            }
        }
        Ok(self
            .sources
            .iter()
            .filter(|source| source.descriptor.active && selected(&source.descriptor, filter))
            .filter_map(data_view)
            .collect())
    }

    #[cfg(test)]
    pub fn limits(&self, filter: &SourceFilter) -> Result<Vec<LimitView<'_>>, String> {
        let mut limits = Vec::new();
        for view in self.selected(filter)? {
            let mut entries: Vec<_> = view.limits.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            limits.extend(entries.into_iter().map(|(agent, snapshot)| LimitView {
                source: view.source,
                agent,
                snapshot,
            }));
        }
        Ok(limits)
    }
}

fn selected(source: &SourceDescriptor, filter: &SourceFilter) -> bool {
    match filter {
        SourceFilter::All => true,
        SourceFilter::Local => source.local,
        SourceFilter::Remote(id) => !source.local && source.id == *id,
    }
}

fn data_view<'a, 'b>(source: &'a DatasetSource<'b>) -> Option<DataView<'a>>
where
    'b: 'a,
{
    match source.data.as_ref()? {
        DataRef::Local(data) => Some(DataView {
            source: &source.descriptor,
            agg: &data.agg,
            hours: &data.hours,
            rounds: &data.rounds,
            limits: &data.limits,
        }),
        DataRef::Remote(data) => Some(DataView {
            source: &source.descriptor,
            agg: &data.agg,
            hours: &data.hours,
            rounds: &data.rounds,
            limits: &data.limits,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::cache::{agg_key, CompactData, CACHE_VERSION};
    use crate::data::config::SshSourceConfig;
    use crate::data::protocol::{ExportRefreshStatus, ProtocolRetention, SourceSnapshot};
    use crate::data::remote_store::AttemptFailureKind;
    use crate::data::timeutil::local_offset;
    use std::path::PathBuf;

    const NOW: i64 = 1_784_317_200;
    const LOCAL_ID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const REMOTE_ID: &str = "223e4567-e89b-12d3-a456-426614174000";

    fn cache() -> Cache {
        Cache::load(PathBuf::from("/does/not/exist"))
    }

    fn source(id: &str, enabled: bool) -> SshSourceConfig {
        SshSourceConfig {
            id: id.into(),
            label: id.to_uppercase(),
            host: id.into(),
            enabled,
            binary: "tokmeter".into(),
        }
    }

    fn snapshot(instance_id: &str, value: u64) -> SourceSnapshot {
        let mut data = CompactData::default();
        data.agg.insert(
            agg_key("2026-07-17", "claude", "opus", "standard", "/same"),
            [1, value, 0, 0, 0, 0],
        );
        SourceSnapshot {
            app_version: "0.1.8".into(),
            cache_version: CACHE_VERSION,
            instance_id: instance_id.into(),
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

    #[test]
    fn local_and_first_unique_remote_are_active_in_config_order() {
        let local = cache();
        let config = Config {
            ssh_sources: vec![
                source("one", true),
                source("alias", true),
                source("off", false),
            ],
            ..Config::default()
        };
        let home = std::env::temp_dir().to_string_lossy().to_string();
        let mut store = RemoteStore::empty(&home);
        store.apply_success("one", "ONE", snapshot(REMOTE_ID, 10), Vec::new(), NOW, 1);
        store.apply_success(
            "alias",
            "ALIAS",
            snapshot(REMOTE_ID, 10),
            Vec::new(),
            NOW,
            1,
        );
        store.apply_success(
            "off",
            "OFF",
            snapshot("323e4567-e89b-12d3-a456-426614174000", 10),
            Vec::new(),
            NOW,
            1,
        );

        let dataset = Dataset::new(&local, &config, Some(LOCAL_ID), &store, NOW);
        let descriptors: Vec<_> = dataset.sources().collect();
        assert_eq!(
            descriptors
                .iter()
                .map(|source| source.id.as_str())
                .collect::<Vec<_>>(),
            vec!["local", "one", "alias", "off"]
        );
        assert!(descriptors[1].active);
        assert_eq!(descriptors[2].health, SourceHealth::DuplicateInstance);
        assert!(!descriptors[2].active);
        assert_eq!(descriptors[3].health, SourceHealth::Disabled);
        assert_eq!(dataset.selected(&SourceFilter::All).unwrap().len(), 2);
    }

    #[test]
    fn local_loopback_is_excluded() {
        let local = cache();
        let config = Config {
            ssh_sources: vec![source("loop", true)],
            ..Config::default()
        };
        let mut store = RemoteStore::empty("/tmp");
        store.apply_success("loop", "LOOP", snapshot(LOCAL_ID, 10), Vec::new(), NOW, 1);
        let dataset = Dataset::new(&local, &config, Some(LOCAL_ID), &store, NOW);
        let loopback = dataset.source("loop").unwrap();
        assert_eq!(loopback.health, SourceHealth::DuplicateInstance);
        assert!(!loopback.active);
    }

    #[test]
    fn age_and_failures_keep_last_good_active() {
        let local = cache();
        let config = Config {
            ssh_sources: vec![source("old", true), source("bad", true)],
            ..Config::default()
        };
        let mut store = RemoteStore::empty("/tmp");
        store.apply_success(
            "old",
            "OLD",
            snapshot(REMOTE_ID, 10),
            Vec::new(),
            NOW - 301,
            1,
        );
        store.apply_success(
            "bad",
            "BAD",
            snapshot("323e4567-e89b-12d3-a456-426614174000", 10),
            Vec::new(),
            NOW - 1,
            1,
        );
        store.apply_failure(
            "bad",
            "BAD",
            AttemptFailureKind::Incompatible,
            "version",
            NOW,
            1,
        );
        let dataset = Dataset::new(&local, &config, Some(LOCAL_ID), &store, NOW);
        assert_eq!(dataset.source("old").unwrap().health, SourceHealth::Stale);
        assert!(dataset.source("old").unwrap().active);
        assert_eq!(
            dataset.source("bad").unwrap().health,
            SourceHealth::Incompatible
        );
        assert!(dataset.source("bad").unwrap().active);
    }

    #[test]
    fn filters_validate_ids_and_keep_limit_identity() {
        let mut local = cache();
        local.limits.insert("claude".into(), Snapshot::default());
        let config = Config {
            ssh_sources: vec![source("one", true)],
            ..Config::default()
        };
        let mut remote = snapshot(REMOTE_ID, 10);
        remote
            .data
            .limits
            .insert("claude".into(), Snapshot::default());
        let mut store = RemoteStore::empty("/tmp");
        store.apply_success("one", "ONE", remote, Vec::new(), NOW, 1);
        let dataset = Dataset::new(&local, &config, Some(LOCAL_ID), &store, NOW);
        let limits = dataset.limits(&SourceFilter::All).unwrap();
        assert_eq!(limits.len(), 2);
        assert!(limits
            .iter()
            .all(|limit| limit.agent == "claude" && limit.snapshot.checked == 0));
        assert_eq!(
            dataset.limits(&SourceFilter::Local).unwrap()[0].source.id,
            "local"
        );
        assert_eq!(
            dataset.limits(&SourceFilter::Remote("one".into())).unwrap()[0]
                .source
                .id,
            "one"
        );
        assert!(dataset
            .selected(&SourceFilter::Remote("missing".into()))
            .is_err());
    }
}

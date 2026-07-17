//! Сводка для панели за выбранный период: по агентам / моделям / топ-проектам,
//! столбчатый график токенов и темп/раунды, с опциональным скоупом на проект.

#[cfg(test)]
use super::cache::{Cache, Round};
use super::cache::{Counts, SEP};
use super::dataset::{DataView, Dataset, SourceFilter};
use super::pricing::Pricing;
use super::timeutil::{civil_from_days, hm, ymd_str, ymd_to_days};
use std::collections::{BTreeMap, HashMap};

#[derive(Default, Clone, Copy)]
pub struct Tot {
    pub req: u64,
    pub inp: u64,   // некэшированный вход
    pub out: u64,   // выход
    pub cache: u64, // cache_read + cache_write(5m/1h)
    pub cost: f64,
}

impl Tot {
    pub fn tokens(&self) -> u64 {
        self.inp + self.out + self.cache
    }
    fn add(&mut self, req: u64, inp: u64, out: u64, cache: u64, cost: f64) {
        self.req += req;
        self.inp += inp;
        self.out += out;
        self.cache += cache;
        self.cost += cost;
    }
}

pub struct Line {
    pub label: String,
    pub tot: Tot,
}

/// Период просмотра. Окно в днях одинаково для секций и для суммы графика.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Timeframe {
    Day,
    Week,
    Month,
    Quarter,
    All,
}

impl Timeframe {
    pub const ALL: [Timeframe; 5] = [
        Timeframe::Day,
        Timeframe::Week,
        Timeframe::Month,
        Timeframe::Quarter,
        Timeframe::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Timeframe::Day => "day",
            Timeframe::Week => "week",
            Timeframe::Month => "month",
            Timeframe::Quarter => "quarter",
            Timeframe::All => "all",
        }
    }

    /// Размер окна в днях (для секций и суммы графика). All — практически без границ.
    fn window_days(self) -> i64 {
        match self {
            Timeframe::Day => 1,
            Timeframe::Week => 7,
            Timeframe::Month => 35,    // 5 недельных столбиков
            Timeframe::Quarter => 91,  // 13 недельных столбиков
            Timeframe::All => 1 << 40, // вся удержанная история
        }
    }

    /// Счётчики раундов живут только в коротком почасовом окне.
    fn rounds_known(self) -> bool {
        matches!(self, Timeframe::Day | Timeframe::Week)
    }
}

/// Столбик графика токенов.
pub struct Bucket {
    pub label: String,
    pub tokens: u64,
}

/// Одна строка вкладки «Раунды».
#[cfg(test)]
#[allow(dead_code)]
pub struct RoundView {
    pub time: String,
    pub agent: String,
    pub model: String,
    pub project: String,
    pub tokens: u64,
    pub cost: f64,
}

/// Сегменты селектора агента на вкладке «Раунды» (индекс 0 = без фильтра).
pub const ROUND_AGENTS: [&str; 4] = ["all", "claude", "codex", "omp"];

pub struct Summary {
    pub agents: Vec<Line>,
    pub agents_total: Tot,
    pub models: Vec<Line>,
    pub chart: Vec<Bucket>,
    // темп / раунды
    pub rate_hour: u64, // токены за текущий локальный час
    pub per_h: f64,     // средние токены/час за период
    pub rounds_total: u64,
    pub per_round: f64,
    pub rounds_known: bool,
}

#[cfg(test)]
pub struct Scope<'a> {
    pub project_root: Option<&'a str>,
}

pub struct SourceScope<'a> {
    pub source_id: Option<&'a str>,
    pub project_root: Option<&'a str>,
}

pub struct SourceLine {
    pub source_id: String,
    pub source_label: String,
    pub path: String,
    pub label: String,
    pub tot: Tot,
}

pub struct SourceRoundView {
    pub source_id: String,
    pub source_label: String,
    pub time: String,
    pub agent: String,
    pub model: String,
    pub project: String,
    pub tokens: u64,
    pub cost: f64,
}

struct DataMaps<'a> {
    agg: &'a HashMap<String, Counts>,
    hours: &'a HashMap<String, [u64; 2]>,
}

impl<'a> DataMaps<'a> {
    #[cfg(test)]
    fn cache(cache: &'a Cache) -> Self {
        Self {
            agg: &cache.agg,
            hours: &cache.hours,
        }
    }

    fn view(view: &'a DataView<'a>) -> Self {
        Self {
            agg: view.agg,
            hours: view.hours,
        }
    }
}

#[cfg(test)]
pub fn build(
    cache: &Cache,
    pricing: &Pricing,
    tf: Timeframe,
    scope: &Scope,
    today_days: i64,
    cur_hour: &str,
    elapsed_h: f64,
) -> Summary {
    build_maps(
        &[DataMaps::cache(cache)],
        pricing,
        tf,
        scope.project_root,
        today_days,
        cur_hour,
        elapsed_h,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_dataset(
    dataset: &Dataset,
    filter: &SourceFilter,
    pricing: &Pricing,
    tf: Timeframe,
    scope: &SourceScope,
    today_days: i64,
    cur_hour: &str,
    elapsed_h: f64,
) -> Result<Summary, String> {
    let views = dataset.selected(filter)?;
    let maps: Vec<_> = views
        .iter()
        .filter(|view| scope.source_id.is_none_or(|id| view.source.id == id))
        .map(DataMaps::view)
        .collect();
    Ok(build_maps(
        &maps,
        pricing,
        tf,
        scope.project_root,
        today_days,
        cur_hour,
        elapsed_h,
    ))
}

fn build_maps(
    maps: &[DataMaps<'_>],
    pricing: &Pricing,
    tf: Timeframe,
    project_root: Option<&str>,
    today_days: i64,
    cur_hour: &str,
    elapsed_h: f64,
) -> Summary {
    let span = tf.window_days();
    let oldest = today_days - span + 1;
    let mut agents: HashMap<String, Tot> = HashMap::new();
    let mut models: HashMap<String, Tot> = HashMap::new();

    for data in maps {
        for (k, c) in data.agg {
            let mut it = k.split(SEP);
            let date = it.next().unwrap_or("");
            let agent = it.next().unwrap_or("");
            let model = it.next().unwrap_or("");
            let speed = it.next().unwrap_or("");
            let project = it.next().unwrap_or("");
            if project_root.is_some_and(|root| !proj_match(project, root)) {
                continue;
            }
            match ymd_to_days(date) {
                Some(d) if d >= oldest && d <= today_days => {}
                _ => continue,
            }
            let inp = c[1];
            let cache_t = c[2] + c[3] + c[4];
            let out = c[5];
            let cost = pricing.cost(model, speed, c[1], c[2], c[3], c[4], c[5]);
            agents
                .entry(agent.to_string())
                .or_default()
                .add(c[0], inp, out, cache_t, cost);
            models
                .entry(model.to_string())
                .or_default()
                .add(c[0], inp, out, cache_t, cost);
        }
    }

    let (mut rate_hour, mut rounds_total) = (0u64, 0u64);
    for data in maps {
        for (k, hc) in data.hours {
            let mut it = k.split(SEP);
            let hour = it.next().unwrap_or("");
            let _agent = it.next().unwrap_or("");
            let project = it.next().unwrap_or("");
            if project_root.is_some_and(|root| !proj_match(project, root)) {
                continue;
            }
            if hour == cur_hour {
                rate_hour += hc[0];
            }
            if tf.rounds_known() {
                let date = if hour.len() >= 10 { &hour[..10] } else { hour };
                if ymd_to_days(date).is_some_and(|d| d >= oldest && d <= today_days) {
                    rounds_total += hc[1];
                }
            }
        }
    }

    let (agents, agents_total) = lines_with_total(agents, usize::MAX);
    let (models, _) = lines_with_total(models, 6);
    let chart = chart_buckets_maps(maps, project_root, tf, today_days);
    let tok = agents_total.tokens() as f64;
    let per_h = if tf == Timeframe::Day {
        if elapsed_h > 0.05 {
            tok / elapsed_h
        } else {
            0.0
        }
    } else {
        tok / (span as f64 * 24.0)
    };
    Summary {
        agents,
        agents_total,
        models,
        chart,
        rate_hour,
        per_h,
        rounds_total,
        per_round: per(tok, rounds_total),
        rounds_known: tf.rounds_known(),
    }
}

/// Последние n раундов (новые сверху) с потокенной стоимостью; agent — фильтр по
/// агенту ("claude"/"codex"), None — без фильтра.
#[cfg(test)]
#[allow(dead_code)]
pub fn rounds_view(
    cache: &Cache,
    pricing: &Pricing,
    off: i64,
    n: usize,
    agent: Option<&str>,
) -> Vec<RoundView> {
    let mut v: Vec<&Round> = cache
        .rounds
        .iter()
        .filter(|r| agent.is_none_or(|a| r.agent == a))
        .collect();
    v.sort_by(|a, b| b.ts.cmp(&a.ts));
    v.into_iter()
        .take(n)
        .map(|r| RoundView {
            time: hm(r.ts, off),
            agent: r.agent.clone(),
            model: r.model.clone(),
            project: short_path(&r.project),
            tokens: r.inp + r.cread + r.cw5 + r.cw1h + r.out,
            cost: pricing.cost(&r.model, &r.speed, r.inp, r.cread, r.cw5, r.cw1h, r.out),
        })
        .collect()
}

/// Топ-проекты за период (по затратам, дороже сверху) плюс суммарный Tot по всем.
/// Глобально, без скоупа — это рейтинг проектов.
pub fn rounds_view_dataset(
    dataset: &Dataset,
    filter: &SourceFilter,
    pricing: &Pricing,
    off: i64,
    n: usize,
    agent: Option<&str>,
) -> Result<Vec<SourceRoundView>, String> {
    let views = dataset.selected(filter)?;
    let mut rounds = Vec::new();
    for view in &views {
        rounds.extend(
            view.rounds
                .iter()
                .filter(|round| agent.is_none_or(|agent| round.agent == agent))
                .map(|round| (view.source, round)),
        );
    }
    rounds.sort_by(|a, b| b.1.ts.cmp(&a.1.ts));
    Ok(rounds
        .into_iter()
        .take(n)
        .map(|(source, round)| SourceRoundView {
            source_id: source.id.clone(),
            source_label: source.label.clone(),
            time: hm(round.ts, off),
            agent: round.agent.clone(),
            model: round.model.clone(),
            project: short_path(&round.project),
            tokens: round.inp + round.cread + round.cw5 + round.cw1h + round.out,
            cost: pricing.cost(
                &round.model,
                &round.speed,
                round.inp,
                round.cread,
                round.cw5,
                round.cw1h,
                round.out,
            ),
        })
        .collect())
}

#[cfg(test)]
#[allow(dead_code)]
pub fn projects_view(
    cache: &Cache,
    pricing: &Pricing,
    tf: Timeframe,
    today_days: i64,
    n: usize,
) -> (Vec<Line>, Tot) {
    let span = tf.window_days();
    let oldest = today_days - span + 1;
    let mut projects: HashMap<String, Tot> = HashMap::new();
    for (k, c) in &cache.agg {
        let mut it = k.split(SEP);
        let date = it.next().unwrap_or("");
        let _agent = it.next().unwrap_or("");
        let model = it.next().unwrap_or("");
        let speed = it.next().unwrap_or("");
        let project = it.next().unwrap_or("");
        match ymd_to_days(date) {
            Some(d) if d >= oldest && d <= today_days => {}
            _ => continue,
        }
        let cost = pricing.cost(model, speed, c[1], c[2], c[3], c[4], c[5]);
        projects.entry(project.to_string()).or_default().add(
            c[0],
            c[1],
            c[5],
            c[2] + c[3] + c[4],
            cost,
        );
    }
    let (mut v, total) = lines_with_total(projects, n);
    for p in &mut v {
        p.label = short_path(&p.label);
    }
    (v, total)
}

pub fn projects_view_dataset(
    dataset: &Dataset,
    filter: &SourceFilter,
    pricing: &Pricing,
    tf: Timeframe,
    today_days: i64,
    n: usize,
) -> Result<(Vec<SourceLine>, Tot), String> {
    let span = tf.window_days();
    let oldest = today_days - span + 1;
    let views = dataset.selected(filter)?;
    let mut projects: HashMap<(String, String, String), Tot> = HashMap::new();
    for view in views {
        for (key, counts) in view.agg {
            let mut fields = key.split(SEP);
            let date = fields.next().unwrap_or("");
            let _agent = fields.next().unwrap_or("");
            let model = fields.next().unwrap_or("");
            let speed = fields.next().unwrap_or("");
            let project = fields.next().unwrap_or("");
            match ymd_to_days(date) {
                Some(day) if day >= oldest && day <= today_days => {}
                _ => continue,
            }
            let cost = pricing.cost(
                model, speed, counts[1], counts[2], counts[3], counts[4], counts[5],
            );
            projects
                .entry((
                    view.source.id.clone(),
                    view.source.label.clone(),
                    project.to_string(),
                ))
                .or_default()
                .add(
                    counts[0],
                    counts[1],
                    counts[5],
                    counts[2] + counts[3] + counts[4],
                    cost,
                );
        }
    }
    let mut total = Tot::default();
    let mut rows: Vec<_> = projects
        .into_iter()
        .map(|((source_id, source_label, path), tot)| {
            total.add(tot.req, tot.inp, tot.out, tot.cache, tot.cost);
            SourceLine {
                source_id,
                source_label,
                label: short_path(&path),
                path,
                tot,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.tot
            .cost
            .partial_cmp(&a.tot.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows.truncate(n);
    Ok((rows, total))
}

/// Столбики токенов слева→справа (старое→новое) под выбранный период.
fn chart_buckets_maps(
    maps: &[DataMaps<'_>],
    project_root: Option<&str>,
    tf: Timeframe,
    today_days: i64,
) -> Vec<Bucket> {
    match tf {
        Timeframe::Day => {
            let today = ymd_str(today_days);
            let mut b = [0u64; 24];
            for data in maps {
                for (k, hc) in data.hours {
                    let mut it = k.split(SEP);
                    let hour = it.next().unwrap_or("");
                    let _agent = it.next().unwrap_or("");
                    let project = it.next().unwrap_or("");
                    if project_root.is_some_and(|root| !proj_match(project, root))
                        || hour.len() < 13
                        || hour[..10] != today
                    {
                        continue;
                    }
                    if let Ok(h) = hour[11..13].parse::<usize>() {
                        if h < 24 {
                            b[h] += hc[0];
                        }
                    }
                }
            }
            (0..24)
                .map(|h| Bucket {
                    label: format!("{h:02}"),
                    tokens: b[h],
                })
                .collect()
        }
        Timeframe::All => {
            let mut map: BTreeMap<String, u64> = BTreeMap::new();
            for data in maps {
                for (k, c) in data.agg {
                    let mut it = k.split(SEP);
                    let date = it.next().unwrap_or("");
                    let _agent = it.next().unwrap_or("");
                    let _model = it.next().unwrap_or("");
                    let _speed = it.next().unwrap_or("");
                    let project = it.next().unwrap_or("");
                    if project_root.is_some_and(|root| !proj_match(project, root)) || date.len() < 7
                    {
                        continue;
                    }
                    *map.entry(date[..7].to_string()).or_insert(0) +=
                        c[1] + c[2] + c[3] + c[4] + c[5];
                }
            }
            map.into_iter()
                .map(|(ym, tokens)| Bucket {
                    label: ym[5..].to_string(),
                    tokens,
                })
                .collect()
        }
        _ => {
            let (n, div) = match tf {
                Timeframe::Week => (7usize, 1i64),
                Timeframe::Month => (5usize, 7i64),
                Timeframe::Quarter => (13usize, 7i64),
                _ => unreachable!(),
            };
            let span = n as i64 * div;
            let mut b = vec![0u64; n];
            for data in maps {
                for (k, c) in data.agg {
                    let mut it = k.split(SEP);
                    let date = it.next().unwrap_or("");
                    let _agent = it.next().unwrap_or("");
                    let _model = it.next().unwrap_or("");
                    let _speed = it.next().unwrap_or("");
                    let project = it.next().unwrap_or("");
                    if project_root.is_some_and(|root| !proj_match(project, root)) {
                        continue;
                    }
                    let Some(day) = ymd_to_days(date) else {
                        continue;
                    };
                    let age = today_days - day;
                    if age < 0 || age >= span {
                        continue;
                    }
                    let index = (n - 1) - (age / div) as usize;
                    b[index] += c[1] + c[2] + c[3] + c[4] + c[5];
                }
            }
            (0..n)
                .map(|i| {
                    let label = if tf == Timeframe::Week {
                        let (_, _, day) = civil_from_days(today_days - (n as i64 - 1 - i as i64));
                        format!("{day:02}")
                    } else {
                        format!("{}", i + 1)
                    };
                    Bucket {
                        label,
                        tokens: b[i],
                    }
                })
                .collect()
        }
    }
}

fn per(tokens: f64, rounds: u64) -> f64 {
    if rounds > 0 {
        tokens / rounds as f64
    } else {
        0.0
    }
}

fn lines_with_total(map: HashMap<String, Tot>, top: usize) -> (Vec<Line>, Tot) {
    let mut total = Tot::default();
    let mut v: Vec<Line> = map
        .into_iter()
        .map(|(label, tot)| {
            total.add(tot.req, tot.inp, tot.out, tot.cache, tot.cost);
            Line { label, tot }
        })
        .collect();
    v.sort_by(|a, b| {
        b.tot
            .cost
            .partial_cmp(&a.tot.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if v.len() > top {
        v.truncate(top);
    }
    (v, total)
}

fn proj_match(project: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    project == root || project.starts_with(&format!("{root}/"))
}

/// Последние два сегмента пути для компактного показа.
pub fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match parts.len() {
        0 => p.to_string(),
        1 => parts[0].to_string(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::cache::{agg_key, CompactData, CACHE_VERSION};
    use crate::data::config::{Config, SshSourceConfig};
    use crate::data::dataset::Dataset;
    use crate::data::protocol::{ExportRefreshStatus, ProtocolRetention, SourceSnapshot};
    use crate::data::remote_store::RemoteStore;
    use crate::data::timeutil::local_offset;
    use std::collections::HashSet;
    use std::path::PathBuf;

    const NOW: i64 = 1_784_317_200;
    const LOCAL_ID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const REMOTE_ID: &str = "223e4567-e89b-12d3-a456-426614174000";

    fn cache() -> Cache {
        Cache::load(PathBuf::from("/does/not/exist"))
    }

    fn config() -> Config {
        Config {
            ssh_sources: vec![SshSourceConfig {
                id: "lxc".into(),
                label: "LXC".into(),
                host: "lxc".into(),
                enabled: true,
                binary: "tokmeter".into(),
            }],
            ..Config::default()
        }
    }

    fn add_data(data: &mut CompactData, tokens: u64, ts: i64) {
        data.agg.insert(
            agg_key("2026-07-17", "claude", "opus", "standard", "/same/project"),
            [1, tokens, 0, 0, 0, 0],
        );
        data.hours.insert(
            format!("2026-07-17 20{SEP}claude{SEP}/same/project"),
            [tokens, 1],
        );
        data.rounds.push(Round {
            ts,
            agent: "claude".into(),
            model: "opus".into(),
            speed: "standard".into(),
            project: "/same/project".into(),
            inp: tokens,
            cread: 0,
            cw5: 0,
            cw1h: 0,
            out: 0,
        });
    }

    fn snapshot(tokens: u64, ts: i64) -> SourceSnapshot {
        let mut data = CompactData::default();
        add_data(&mut data, tokens, ts);
        SourceSnapshot {
            app_version: "0.1.8".into(),
            cache_version: CACHE_VERSION,
            instance_id: REMOTE_ID.into(),
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

    fn dataset<'a>(local: &'a Cache, config: &'a Config, store: &'a RemoteStore) -> Dataset<'a> {
        Dataset::new(local, config, Some(LOCAL_ID), store, NOW)
    }

    #[test]
    fn local_dataset_matches_existing_aggregation() {
        let mut local = cache();
        let mut data = CompactData::default();
        add_data(&mut data, 10, NOW);
        local.agg = data.agg;
        local.hours = data.hours;
        local.rounds = data.rounds;
        let config = Config::default();
        let store = RemoteStore::empty("/tmp");
        let dataset = dataset(&local, &config, &store);
        let pricing = Pricing::load("{}");
        let day = ymd_to_days("2026-07-17").unwrap();
        let current = build(
            &local,
            &pricing,
            Timeframe::Day,
            &Scope { project_root: None },
            day,
            "2026-07-17 20",
            12.0,
        );
        let composed = build_dataset(
            &dataset,
            &SourceFilter::Local,
            &pricing,
            Timeframe::Day,
            &SourceScope {
                source_id: None,
                project_root: None,
            },
            day,
            "2026-07-17 20",
            12.0,
        )
        .unwrap();
        assert_eq!(
            composed.agents_total.tokens(),
            current.agents_total.tokens()
        );
        assert_eq!(composed.rate_hour, current.rate_hour);
        assert_eq!(composed.rounds_total, current.rounds_total);
        assert_eq!(
            composed
                .chart
                .iter()
                .map(|bucket| bucket.tokens)
                .collect::<Vec<_>>(),
            current
                .chart
                .iter()
                .map(|bucket| bucket.tokens)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn combined_views_sum_totals_and_retain_source_identity() {
        let mut local = cache();
        let mut local_data = CompactData::default();
        add_data(&mut local_data, 10, NOW - 10);
        local.agg = local_data.agg;
        local.hours = local_data.hours;
        local.rounds = local_data.rounds;
        let config = config();
        let mut store = RemoteStore::empty("/tmp");
        store.apply_success("lxc", "LXC", snapshot(20, NOW), Vec::new(), NOW, 1);
        let dataset = dataset(&local, &config, &store);
        let pricing = Pricing::load("{}");
        let day = ymd_to_days("2026-07-17").unwrap();
        let summary = build_dataset(
            &dataset,
            &SourceFilter::All,
            &pricing,
            Timeframe::Day,
            &SourceScope {
                source_id: None,
                project_root: None,
            },
            day,
            "2026-07-17 20",
            12.0,
        )
        .unwrap();
        assert_eq!(summary.agents_total.tokens(), 30);
        assert_eq!(summary.rate_hour, 30);
        assert_eq!(summary.rounds_total, 2);

        let (projects, total) = projects_view_dataset(
            &dataset,
            &SourceFilter::All,
            &pricing,
            Timeframe::Day,
            day,
            10,
        )
        .unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(total.tokens(), 30);
        assert_eq!(
            projects
                .iter()
                .map(|project| project.source_id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["local", "lxc"])
        );
        assert!(projects
            .iter()
            .all(|project| project.path == "/same/project"));

        let rounds = rounds_view_dataset(
            &dataset,
            &SourceFilter::All,
            &pricing,
            local_offset(NOW),
            10,
            None,
        )
        .unwrap();
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[0].source_id, "lxc");
        assert_eq!(rounds[1].source_id, "local");
    }
}

//! Сводка для панели за выбранный период: по агентам / моделям / топ-проектам,
//! столбчатый график токенов и темп/раунды, с опциональным скоупом на проект.

use super::cache::{Cache, Round, SEP};
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
pub struct RoundView {
    pub time: String,
    pub agent: String,
    pub project: String,
    pub tokens: u64,
    pub cost: f64,
}

/// Сегменты селектора агента на вкладке «Раунды» (индекс 0 = без фильтра).
pub const ROUND_AGENTS: [&str; 3] = ["all", "claude", "codex"];

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

pub struct Scope<'a> {
    pub project_root: Option<&'a str>,
}

pub fn build(
    cache: &Cache,
    pricing: &Pricing,
    tf: Timeframe,
    scope: &Scope,
    today_days: i64,
    cur_hour: &str,
    elapsed_h: f64,
) -> Summary {
    let span = tf.window_days();
    let oldest = today_days - span + 1; // включительно

    let mut agents: HashMap<String, Tot> = HashMap::new();
    let mut models: HashMap<String, Tot> = HashMap::new();

    for (k, c) in &cache.agg {
        let mut it = k.split(SEP);
        let date = it.next().unwrap_or("");
        let agent = it.next().unwrap_or("");
        let model = it.next().unwrap_or("");
        let speed = it.next().unwrap_or("");
        let project = it.next().unwrap_or("");

        if let Some(root) = scope.project_root {
            if !proj_match(project, root) {
                continue;
            }
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

    // темп (текущий час) и раунды (в окне) из почасового агрегата
    let (mut rate_hour, mut rounds_total) = (0u64, 0u64);
    for (k, hc) in &cache.hours {
        let mut it = k.split(SEP);
        let hour = it.next().unwrap_or(""); // "YYYY-MM-DD HH"
        let _agent = it.next().unwrap_or("");
        let project = it.next().unwrap_or("");
        if let Some(root) = scope.project_root {
            if !proj_match(project, root) {
                continue;
            }
        }
        if hour == cur_hour {
            rate_hour += hc[0];
        }
        if tf.rounds_known() {
            let date = if hour.len() >= 10 { &hour[..10] } else { hour };
            if let Some(d) = ymd_to_days(date) {
                if d >= oldest && d <= today_days {
                    rounds_total += hc[1];
                }
            }
        }
    }

    let (agents, agents_total) = lines_with_total(agents, usize::MAX);
    let (models, _) = lines_with_total(models, 6);

    let chart = chart_buckets(cache, scope, tf, today_days);

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
            project: short_path(&r.project),
            tokens: r.inp + r.cread + r.cw5 + r.cw1h + r.out,
            cost: pricing.cost(&r.model, &r.speed, r.inp, r.cread, r.cw5, r.cw1h, r.out),
        })
        .collect()
}

/// Топ-проекты за период (по затратам, дороже сверху) плюс суммарный Tot по всем.
/// Глобально, без скоупа — это рейтинг проектов.
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

/// Столбики токенов слева→справа (старое→новое) под выбранный период.
fn chart_buckets(cache: &Cache, scope: &Scope, tf: Timeframe, today_days: i64) -> Vec<Bucket> {
    match tf {
        // 24 часовых столбика за сегодня — из почасового агрегата.
        Timeframe::Day => {
            let today = ymd_str(today_days);
            let mut b = [0u64; 24];
            for (k, hc) in &cache.hours {
                let mut it = k.split(SEP);
                let hour = it.next().unwrap_or("");
                let _agent = it.next().unwrap_or("");
                let project = it.next().unwrap_or("");
                if let Some(root) = scope.project_root {
                    if !proj_match(project, root) {
                        continue;
                    }
                }
                if hour.len() < 13 || hour[..10] != today {
                    continue;
                }
                if let Ok(h) = hour[11..13].parse::<usize>() {
                    if h < 24 {
                        b[h] += hc[0];
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
        // Помесячные столбики по всей удержанной истории.
        Timeframe::All => {
            let mut map: BTreeMap<String, u64> = BTreeMap::new();
            for (k, c) in &cache.agg {
                let mut it = k.split(SEP);
                let date = it.next().unwrap_or("");
                let _agent = it.next().unwrap_or("");
                let _model = it.next().unwrap_or("");
                let _speed = it.next().unwrap_or("");
                let project = it.next().unwrap_or("");
                if let Some(root) = scope.project_root {
                    if !proj_match(project, root) {
                        continue;
                    }
                }
                if date.len() < 7 {
                    continue;
                }
                *map.entry(date[..7].to_string()).or_insert(0) += c[1] + c[2] + c[3] + c[4] + c[5];
            }
            map.into_iter()
                .map(|(ym, t)| Bucket {
                    label: ym[5..].to_string(),
                    tokens: t,
                })
                .collect()
        }
        // Дневные (неделя) или недельные (месяц/квартал) столбики из дневного агрегата.
        _ => {
            let (n, div) = match tf {
                Timeframe::Week => (7usize, 1i64),
                Timeframe::Month => (5usize, 7i64),
                Timeframe::Quarter => (13usize, 7i64),
                _ => unreachable!(),
            };
            let span = n as i64 * div;
            let mut b = vec![0u64; n];
            for (k, c) in &cache.agg {
                let mut it = k.split(SEP);
                let date = it.next().unwrap_or("");
                let _agent = it.next().unwrap_or("");
                let _model = it.next().unwrap_or("");
                let _speed = it.next().unwrap_or("");
                let project = it.next().unwrap_or("");
                if let Some(root) = scope.project_root {
                    if !proj_match(project, root) {
                        continue;
                    }
                }
                let dd = match ymd_to_days(date) {
                    Some(d) => d,
                    None => continue,
                };
                let age = today_days - dd;
                if age < 0 || age >= span {
                    continue;
                }
                let idx = (n - 1) - (age / div) as usize;
                b[idx] += c[1] + c[2] + c[3] + c[4] + c[5];
            }
            (0..n)
                .map(|i| {
                    let label = if tf == Timeframe::Week {
                        let (_, _, d) = civil_from_days(today_days - (n as i64 - 1 - i as i64));
                        format!("{d:02}")
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

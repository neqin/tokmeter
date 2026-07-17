//! GPUI tokmeter — token spend panel with real tok data layer.

mod data;

use data::agg::{self, SourceScope, Timeframe, Tot};
use data::cache::{Cache, CACHE_VERSION};
use data::config::{self, Config};
use data::dataset::{Dataset, SourceFilter};
use data::engine::{self, LocalRefreshOutcome};
use data::identity;
use data::limits;
use data::pricing::Pricing;
use data::protocol::{ProtocolRetention, SourceSnapshot, SourceWarning};
use data::remote::RemoteCoordinator;
use data::remote_store::{RemoteStore, SourceHealth};
use data::timeutil::{
    clock, local_day, local_offset, now_epoch, secs_into_local_day, ymd_hour_str,
};
use gpui::{
    actions, canvas, div, fill, point, prelude::*, px, relative, rgb, size, AnyElement, App,
    AppContext, Bounds, Context, FocusHandle, Focusable, InteractiveElement, KeyBinding,
    ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use std::env;
use std::sync::{Arc, Mutex};

// ── Theme ───────────────────────────────────────────────────────────────────

fn bg() -> gpui::Rgba {
    rgb(0x0e0e10)
}
fn text() -> gpui::Rgba {
    rgb(0xe8e4dc)
}
fn muted() -> gpui::Rgba {
    rgb(0x9a9186)
}
fn dim() -> gpui::Rgba {
    rgb(0x6e665c)
}
fn accent() -> gpui::Rgba {
    rgb(0xf0a030)
}
fn pill_fg() -> gpui::Rgba {
    rgb(0x1a1208)
}
fn cost_hi() -> gpui::Rgba {
    rgb(0xff6b6b)
}
fn cost_lo() -> gpui::Rgba {
    rgb(0x5ecf8a)
}
fn bar_colors() -> [u32; 6] {
    [0x8b4513, 0xb85c1a, 0xd97706, 0xea8c10, 0xf5a623, 0xffc107]
}

const MONO: &str = "JetBrains Mono";
const UI_W: f32 = 560.0;
const UI_H: f32 = 920.0;
const CHART_H: f32 = 140.0;

// ── Owned snapshot (paint reads only this) ──────────────────────────────────

#[derive(Clone, Default)]
struct AgentRowOwned {
    name: String,
    req: u64,
    input: u64,
    output: u64,
    cache: u64,
    cost: f64,
}

#[derive(Clone, Default)]
struct ModelRowOwned {
    name: String,
    req: u64,
    input: u64,
    output: u64,
    cache: u64,
    tokens: u64,
    cost: f64,
}

#[derive(Clone, Default)]
struct ProjectRowOwned {
    source_id: String,
    source_label: String,
    path: String,
    name: String,
    tokens: u64,
    cost: f64,
}

#[derive(Clone, Default)]
struct RoundRowOwned {
    source_id: String,
    source_label: String,
    time: String,
    agent: String,
    model: String,
    project: String,
    tokens: u64,
    cost: f64,
}

#[derive(Clone, Default)]
struct LimitWinOwned {
    label: String,
    pct: f64,
}

#[derive(Clone, Default)]
struct LimitRowOwned {
    source_id: String,
    source_label: String,
    agent: String,
    windows: Vec<LimitWinOwned>,
    age: i64,
}

#[derive(Clone)]
struct SourceOwned {
    id: String,
    label: String,
    local: bool,
    enabled: bool,
    active: bool,
    health: SourceHealth,
    warnings: Vec<SourceWarning>,
    last_attempt: i64,
    last_success: i64,
    duration_ms: u64,
    error: String,
}

#[derive(Clone, Default)]
struct DiagnosticsOwned {
    config: Option<String>,
    identity: Option<String>,
    remote_store: Option<String>,
}

#[derive(Clone, Default)]
struct ChartBar {
    label: String,
    tokens: u64,
}

#[derive(Clone)]
struct ViewSnapshot {
    tf: Timeframe,
    total_tokens: u64,
    total_cost: f64,
    rate_hour: u64,
    per_h: f64,
    rounds_total: u64,
    per_round: f64,
    rounds_known: bool,
    chart: Vec<ChartBar>,
    agents: Vec<AgentRowOwned>,
    models: Vec<ModelRowOwned>,
    limits: Vec<LimitRowOwned>,
    projects_tab: Vec<ProjectRowOwned>,
    projects_total: Tot,
    rounds: Vec<RoundRowOwned>,
    sources: Vec<SourceOwned>,
    diagnostics: DiagnosticsOwned,
    clock: String,
}

impl Default for ViewSnapshot {
    fn default() -> Self {
        Self {
            tf: Timeframe::Week,
            total_tokens: 0,
            total_cost: 0.0,
            rate_hour: 0,
            per_h: 0.0,
            rounds_total: 0,
            per_round: 0.0,
            rounds_known: true,
            chart: Vec::new(),
            agents: Vec::new(),
            models: Vec::new(),
            limits: Vec::new(),
            projects_tab: Vec::new(),
            projects_total: Tot::default(),
            rounds: Vec::new(),
            sources: Vec::new(),
            diagnostics: DiagnosticsOwned::default(),
            clock: String::new(),
        }
    }
}

fn build_snapshot(
    dataset: &Dataset,
    source_filter: &SourceFilter,
    pricing: &Pricing,
    tf: Timeframe,
    round_agent_idx: usize,
    diagnostics: DiagnosticsOwned,
) -> Result<ViewSnapshot, String> {
    let now = now_epoch();
    let off = local_offset(now);
    let today = local_day(now, off);
    let cur_hour = ymd_hour_str(now, off);
    let elapsed_h = secs_into_local_day(now, off) as f64 / 3600.0;
    let scope = SourceScope {
        source_id: None,
        project_root: None,
    };
    let summary = agg::build_dataset(
        dataset,
        source_filter,
        pricing,
        tf,
        &scope,
        today,
        &cur_hour,
        elapsed_h,
    )?;
    let agents = summary
        .agents
        .iter()
        .map(|agent| AgentRowOwned {
            name: agent.label.clone(),
            req: agent.tot.req,
            input: agent.tot.inp,
            output: agent.tot.out,
            cache: agent.tot.cache,
            cost: agent.tot.cost,
        })
        .collect();
    let models = summary
        .models
        .iter()
        .filter(|model| !is_synthetic_label(&model.label))
        .map(|model| ModelRowOwned {
            name: model.label.clone(),
            req: model.tot.req,
            input: model.tot.inp,
            output: model.tot.out,
            cache: model.tot.cache,
            tokens: model.tot.tokens(),
            cost: model.tot.cost,
        })
        .collect();
    let chart = summary
        .chart
        .iter()
        .map(|bucket| ChartBar {
            label: bucket.label.clone(),
            tokens: bucket.tokens,
        })
        .collect();
    let (projects, projects_total) =
        agg::projects_view_dataset(dataset, source_filter, pricing, tf, today, 14)?;
    let projects_tab = projects
        .into_iter()
        .map(|project| ProjectRowOwned {
            source_id: project.source_id,
            source_label: project.source_label,
            path: project.path,
            name: project.label,
            tokens: project.tot.tokens(),
            cost: project.tot.cost,
        })
        .collect();
    let round_filter = if round_agent_idx == 0 {
        None
    } else {
        Some(agg::ROUND_AGENTS[round_agent_idx])
    };
    let rounds = agg::rounds_view_dataset(dataset, source_filter, pricing, off, 20, round_filter)?
        .into_iter()
        .map(|round| RoundRowOwned {
            source_id: round.source_id,
            source_label: round.source_label,
            time: round.time,
            agent: round.agent,
            model: round.model,
            project: round.project,
            tokens: round.tokens,
            cost: round.cost,
        })
        .collect();
    let mut limits_owned = Vec::new();
    for view in dataset.selected(source_filter)? {
        limits_owned.extend(
            limits::rows(|agent| view.limits.get(agent).cloned(), now)
                .into_iter()
                .map(|row| LimitRowOwned {
                    source_id: view.source.id.clone(),
                    source_label: view.source.label.clone(),
                    agent: row.agent.to_string(),
                    windows: row
                        .windows
                        .into_iter()
                        .map(|window| LimitWinOwned {
                            label: window.label,
                            pct: window.pct,
                        })
                        .collect(),
                    age: row.age,
                }),
        );
    }
    let sources = dataset
        .sources()
        .map(|source| SourceOwned {
            id: source.id.clone(),
            label: source.label.clone(),
            local: source.local,
            enabled: source.enabled,
            active: source.active,
            health: source.health,
            warnings: source.warnings.clone(),
            last_attempt: source.last_attempt,
            last_success: source.last_success,
            duration_ms: source.duration_ms,
            error: source.error.clone(),
        })
        .collect();

    Ok(ViewSnapshot {
        tf,
        total_tokens: summary.agents_total.tokens(),
        total_cost: summary.agents_total.cost,
        rate_hour: summary.rate_hour,
        per_h: summary.per_h,
        rounds_total: summary.rounds_total,
        per_round: summary.per_round,
        rounds_known: summary.rounds_known,
        chart,
        agents,
        models,
        limits: limits_owned,
        projects_tab,
        projects_total,
        rounds,
        sources,
        diagnostics,
        clock: clock(now, off),
    })
}

fn health_name(health: SourceHealth) -> &'static str {
    match health {
        SourceHealth::Disabled => "disabled",
        SourceHealth::Connecting => "connecting",
        SourceHealth::Healthy => "healthy",
        SourceHealth::Stale => "stale",
        SourceHealth::Error => "error",
        SourceHealth::Incompatible => "incompatible",
        SourceHealth::DuplicateInstance => "duplicate_instance",
    }
}

fn warning_name(warning: SourceWarning) -> &'static str {
    match warning {
        SourceWarning::PartialHistory => "partial_history",
        SourceWarning::ReadOnlyRefresh => "read_only_refresh",
    }
}

fn snapshot_to_json(snap: &ViewSnapshot, mode: &str) -> serde_json::Value {
    let agents: Vec<_> = snap
        .agents
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "tokens": a.input.saturating_add(a.output).saturating_add(a.cache),
                "cost": a.cost,
                "req": a.req,
            })
        })
        .collect();
    let models: Vec<_> = snap
        .models
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "req": m.req,
                "input": m.input,
                "output": m.output,
                "cache": m.cache,
                "tokens": m.tokens,
                "cost": m.cost,
            })
        })
        .collect();
    let projects: Vec<_> = snap
        .projects_tab
        .iter()
        .map(|p| {
            serde_json::json!({
                "source_id": p.source_id,
                "source_label": p.source_label,
                "path": p.path,
                "name": p.name,
                "tokens": p.tokens,
                "cost": p.cost,
            })
        })
        .collect();
    let rounds: Vec<_> = snap
        .rounds
        .iter()
        .map(|r| {
            serde_json::json!({
                "source_id": r.source_id,
                "source_label": r.source_label,
                "time": r.time,
                "agent": r.agent,
                "model": r.model,
                "project": r.project,
                "tokens": r.tokens,
                "cost": r.cost,
            })
        })
        .collect();
    let limits: Vec<_> = snap
        .limits
        .iter()
        .map(|l| {
            serde_json::json!({
                "source_id": l.source_id,
                "source_label": l.source_label,
                "agent": l.agent,
                "age": l.age,
                "windows": l.windows.iter().map(|w| serde_json::json!({
                    "label": w.label, "pct": w.pct
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let sources: Vec<_> = snap
        .sources
        .iter()
        .map(|source| {
            serde_json::json!({
                "id": source.id,
                "label": source.label,
                "local": source.local,
                "enabled": source.enabled,
                "active": source.active,
                "health": health_name(source.health),
                "warnings": source.warnings.iter().map(|warning| warning_name(*warning)).collect::<Vec<_>>(),
                "last_attempt": source.last_attempt,
                "last_success": source.last_success,
                "duration_ms": source.duration_ms,
                "error": source.error,
            })
        })
        .collect();
    let diagnostics = serde_json::json!({
        "config": snap.diagnostics.config,
        "identity": snap.diagnostics.identity,
        "remote_store": snap.diagnostics.remote_store,
    });

    match mode {
        "projects" => serde_json::json!({
            "tf": snap.tf.label(),
            "total_tokens": snap.projects_total.tokens(),
            "total_cost": snap.projects_total.cost,
            "projects": projects,
            "diagnostics": diagnostics,
        }),
        "rounds" => serde_json::json!({
            "tf": snap.tf.label(),
            "rounds": rounds,
            "diagnostics": diagnostics,
        }),
        "limits" => serde_json::json!({
            "limits": limits,
            "diagnostics": diagnostics,
        }),
        "sources" => serde_json::json!({
            "sources": sources,
            "diagnostics": diagnostics,
        }),
        _ => serde_json::json!({
            "tf": snap.tf.label(),
            "total_tokens": snap.total_tokens,
            "total_cost": snap.total_cost,
            "agents": agents,
            "models": models,
            "projects": projects,
            "rounds": rounds,
            "limits": limits,
            "sources": sources,
            "diagnostics": diagnostics,
        }),
    }
}

// ── Format helpers ──────────────────────────────────────────────────────────

fn is_synthetic_label(label: &str) -> bool {
    let t = label.trim();
    t.eq_ignore_ascii_case("<synthetic>") || t.eq_ignore_ascii_case("synthetic")
}

fn ftok(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        format!("{n}")
    }
}

fn fcost(c: f64) -> String {
    if c == 0.0 {
        "$0.00".into()
    } else {
        format!("${c:.2}")
    }
}

/// Day baselines: green < $25, lime < $50, yellow < $100, orange < $200, else red.
/// Each coarser timeframe multiplies thresholds by 4 (week×4, month×16, …).
fn cost_scale(tf: Timeframe) -> f64 {
    match tf {
        Timeframe::Day => 1.0,
        Timeframe::Week => 4.0,
        Timeframe::Month => 16.0,
        Timeframe::Quarter => 64.0,
        // Unbounded history — keep quarter scale so totals don't all go red.
        Timeframe::All => 64.0,
    }
}

fn cost_color(c: f64, tf: Timeframe) -> gpui::Rgba {
    let s = cost_scale(tf);
    if c < 25.0 * s {
        cost_lo() // green
    } else if c < 50.0 * s {
        rgb(0x9fd34a) // lime
    } else if c < 100.0 * s {
        rgb(0xf0c040) // yellow — from $50/day
    } else if c < 200.0 * s {
        rgb(0xf0a030) // orange
    } else {
        cost_hi() // red — from $200/day
    }
}

fn bar_color(level: f32) -> gpui::Rgba {
    let colors = bar_colors();
    let i = ((level.clamp(0.0, 1.0) * (colors.len() - 1) as f32).round() as usize)
        .min(colors.len() - 1);
    rgb(colors[i])
}

/// Usage bar fill: green (low) → yellow → orange → red (high).
fn usage_fill_color(pct: f64) -> gpui::Rgba {
    match pct {
        p if p < 35.0 => rgb(0x3dd68c), // green
        p if p < 55.0 => rgb(0x9fd34a), // lime
        p if p < 70.0 => rgb(0xf0c040), // yellow
        p if p < 85.0 => rgb(0xf0a030), // orange
        _ => rgb(0xff5c5c),             // red
    }
}

fn agent_name_color(agent: &str) -> gpui::Rgba {
    match agent {
        "claude" => rgb(0xf5c542),
        "codex" => accent(),
        "omp" => rgb(0xc084fc),
        "grok" => rgb(0x63c7b2),
        _ => text(),
    }
}

// ── Tabs / keys ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Global,
    Projects,
    Rounds,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Global, Tab::Projects, Tab::Rounds];
    fn label(self) -> &'static str {
        match self {
            Tab::Global => "Usage",
            Tab::Projects => "Projects",
            Tab::Rounds => "Rounds",
        }
    }
    fn next(self) -> Tab {
        match self {
            Tab::Global => Tab::Projects,
            Tab::Projects => Tab::Rounds,
            Tab::Rounds => Tab::Global,
        }
    }
    fn prev(self) -> Tab {
        match self {
            Tab::Global => Tab::Rounds,
            Tab::Projects => Tab::Global,
            Tab::Rounds => Tab::Projects,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DashboardKey {
    TabNext,
    TabPrev,
    Left,
    Right,
    SourceNext,
    SourcePrev,
    Refresh,
}

actions!(
    tokmeter,
    [
        NextTab,
        PrevTab,
        PeriodLeft,
        PeriodRight,
        SourceNext,
        SourcePrev,
        ForceRefresh
    ]
);

// ── Dashboard ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct RemoteRefreshState {
    in_flight: bool,
    force: bool,
}

impl RemoteRefreshState {
    fn request_force(&mut self) {
        self.force = true;
    }

    fn begin(&mut self, has_work: bool) -> bool {
        if self.in_flight || !has_work {
            return false;
        }
        self.in_flight = true;
        self.force = false;
        true
    }

    fn finish(&mut self) -> bool {
        self.in_flight = false;
        self.force
    }
}

fn source_filter_options(sources: &[SourceOwned]) -> Vec<SourceFilter> {
    let mut filters = vec![SourceFilter::All, SourceFilter::Local];
    filters.extend(
        sources
            .iter()
            .filter(|source| !source.local)
            .map(|source| SourceFilter::Remote(source.id.clone())),
    );
    filters
}

fn cycle_source_filter(
    current: &SourceFilter,
    sources: &[SourceOwned],
    forward: bool,
) -> SourceFilter {
    let filters = source_filter_options(sources);
    let current = filters
        .iter()
        .position(|filter| filter == current)
        .unwrap_or(0);
    let next = if forward {
        (current + 1) % filters.len()
    } else if current == 0 {
        filters.len() - 1
    } else {
        current - 1
    };
    filters[next].clone()
}

fn source_identity_visible(filter: &SourceFilter) -> bool {
    matches!(filter, SourceFilter::All)
}

fn source_health_icon(health: SourceHealth) -> &'static str {
    match health {
        SourceHealth::Disabled => "○",
        SourceHealth::Connecting => "◌",
        SourceHealth::Healthy => "●",
        SourceHealth::Stale => "◐",
        SourceHealth::Error => "×",
        SourceHealth::Incompatible => "!",
        SourceHealth::DuplicateInstance => "≡",
    }
}

fn source_health_color(health: SourceHealth) -> gpui::Rgba {
    match health {
        SourceHealth::Healthy => cost_lo(),
        SourceHealth::Connecting => accent(),
        SourceHealth::Stale | SourceHealth::Incompatible => rgb(0xe6a23c),
        SourceHealth::Error => cost_hi(),
        SourceHealth::Disabled | SourceHealth::DuplicateInstance => dim(),
    }
}

fn source_age(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn source_health_text(source: &SourceOwned, now: i64) -> String {
    let mut parts = vec![health_name(source.health).replace('_', " ")];
    if source.last_success > 0
        && matches!(
            source.health,
            SourceHealth::Stale | SourceHealth::Incompatible
        )
    {
        parts.push(format!("{} old", source_age(now - source.last_success)));
    }
    for warning in &source.warnings {
        parts.push(warning_name(*warning).replace('_', " "));
    }
    if !source.error.is_empty()
        && matches!(
            source.health,
            SourceHealth::Stale | SourceHealth::Error | SourceHealth::Incompatible
        )
    {
        parts.push(source.error.clone());
    }
    parts.join(" · ")
}

fn remote_sources_due(
    config: &Config,
    store: &RemoteStore,
    now: i64,
    force: bool,
    identity_available: bool,
) -> Vec<data::config::SshSourceConfig> {
    if !identity_available {
        return Vec::new();
    }
    config
        .ssh_sources
        .iter()
        .filter(|source| {
            if !source.enabled {
                return false;
            }
            if force {
                return true;
            }
            let last_attempt = store
                .sources
                .get(&source.id)
                .map(|stored| stored.last_attempt)
                .unwrap_or(0);
            last_attempt == 0 || now.saturating_sub(last_attempt) >= config.refresh.remote_secs
        })
        .cloned()
        .collect()
}

struct Dashboard {
    focus_handle: FocusHandle,
    home: String,
    pricing: Arc<Pricing>,
    config: Config,
    cache: Cache,
    remote_store: RemoteStore,
    remote_coordinator: RemoteCoordinator,
    remote_refresh: RemoteRefreshState,
    local_instance_id: Option<String>,
    source_filter: SourceFilter,
    diagnostics: DiagnosticsOwned,
    snapshot: ViewSnapshot,
    tab: Tab,
    period: Timeframe,
    round_agent_idx: usize,
    status: Option<String>,
    refresh_in_flight: bool,
    force_refresh: bool,
    limits_ttl: i64,
    refresh_secs: i64,
}

impl Dashboard {
    fn new(cx: &mut Context<Self>) -> Self {
        let home = env::var("HOME").unwrap_or_default();
        let pricing = Arc::new(Pricing::load(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/pricing.json"
        ))));
        let loaded = config::load(&home);
        let config_error = loaded
            .error
            .as_ref()
            .map(|error| format!("{}: {error}", loaded.path.display()));
        let config = loaded.config;
        let (local_instance_id, identity_error) = match identity::load_or_create(&home) {
            Ok(instance_id) => (Some(instance_id), None),
            Err(error) => (
                None,
                Some(format!("installation identity unavailable: {error}")),
            ),
        };
        let retention = ProtocolRetention {
            history_days: config.retention.history_days,
            hours_days: config.retention.hours_days,
        };
        let remote_store = RemoteStore::load(&home, now_epoch(), retention);
        let diagnostics = DiagnosticsOwned {
            config: config_error,
            identity: identity_error,
            remote_store: remote_store.load_error.clone(),
        };
        let limits_ttl = config.refresh.limits_ttl_secs;
        let refresh_secs = config.refresh.ui_secs;
        let focus_handle = cx.focus_handle();
        let cache = Cache::load(engine::cache_path(&home));

        let mut dash = Self {
            focus_handle,
            home,
            pricing,
            config,
            cache,
            remote_store,
            remote_coordinator: RemoteCoordinator::default(),
            remote_refresh: RemoteRefreshState::default(),
            local_instance_id,
            source_filter: SourceFilter::All,
            diagnostics,
            snapshot: ViewSnapshot::default(),
            tab: Tab::Global,
            period: Timeframe::Week,
            round_agent_idx: 0,
            status: Some("scanning…".into()),
            refresh_in_flight: true,
            force_refresh: false,
            limits_ttl,
            refresh_secs,
        };
        dash.rebuild_from_cache();
        dash.schedule_reload(cx, true);
        dash.schedule_remote_refresh(cx);
        dash.start_refresh_loop(cx);
        dash.start_remote_refresh_loop(cx);
        dash
    }

    fn schedule_reload(&mut self, cx: &mut Context<Self>, first: bool) {
        if self.refresh_in_flight && !first {
            return;
        }
        self.refresh_in_flight = true;
        self.force_refresh = false;
        if first {
            self.status = Some("scanning…".into());
        }
        let home = self.home.clone();
        let limits_ttl = self.limits_ttl;
        let config = self.config.clone();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    engine::reload_configured(&home, &config, limits_ttl, false)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.cache = outcome.cache;
                this.rebuild_from_cache();
                this.refresh_in_flight = false;
                let rerun = this.force_refresh;
                this.force_refresh = false;
                this.status = None;
                if rerun {
                    this.schedule_reload(cx, false);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_refresh_loop(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let Some(secs) = this
                .update(cx, |this, _| this.refresh_secs.max(1) as u64)
                .ok()
            else {
                break;
            };
            cx.background_executor()
                .timer(std::time::Duration::from_secs(secs))
                .await;
            let cont = this
                .update(cx, |this, cx| {
                    if this.force_refresh || !this.refresh_in_flight {
                        this.schedule_reload(cx, false);
                    }
                    true
                })
                .unwrap_or(false);
            if !cont {
                break;
            }
        })
        .detach();
    }

    fn schedule_remote_refresh(&mut self, cx: &mut Context<Self>) {
        let force = self.remote_refresh.force;
        let sources = remote_sources_due(
            &self.config,
            &self.remote_store,
            now_epoch(),
            force,
            self.local_instance_id.is_some(),
        );
        if !self.remote_refresh.begin(!sources.is_empty()) {
            if sources.is_empty() && !self.remote_refresh.in_flight {
                self.remote_refresh.force = false;
            }
            return;
        }
        let retention = ProtocolRetention {
            history_days: self.config.retention.history_days,
            hours_days: self.config.retention.hours_days,
        };
        let receiver = self.remote_coordinator.start(
            sources,
            self.config.refresh,
            retention,
            &mut self.remote_store,
        );
        self.rebuild_from_cache();
        cx.notify();
        let receiver = Arc::new(Mutex::new(receiver));
        cx.spawn(async move |this, cx| {
            loop {
                let receiver = receiver.clone();
                let result = cx
                    .background_spawn(async move { receiver.lock().unwrap().recv().ok() })
                    .await;
                let Some(result) = result else {
                    break;
                };
                let _ = this.update(cx, |this, cx| {
                    if this
                        .remote_coordinator
                        .apply(result, &mut this.remote_store)
                    {
                        match this.remote_store.save() {
                            Ok(()) => {
                                this.diagnostics.remote_store =
                                    this.remote_store.load_error.clone();
                            }
                            Err(error) => {
                                this.diagnostics.remote_store =
                                    Some(format!("remote cache save failed: {error}"));
                            }
                        }
                        this.rebuild_from_cache();
                        cx.notify();
                    }
                });
            }
            let _ = this.update(cx, |this, cx| {
                let rerun = this.remote_refresh.finish();
                if rerun {
                    this.schedule_remote_refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_remote_refresh_loop(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let Some(secs) = this
                .update(cx, |this, _| this.config.refresh.remote_secs.max(1) as u64)
                .ok()
            else {
                break;
            };
            cx.background_executor()
                .timer(std::time::Duration::from_secs(secs))
                .await;
            let cont = this
                .update(cx, |this, cx| {
                    this.schedule_remote_refresh(cx);
                    true
                })
                .unwrap_or(false);
            if !cont {
                break;
            }
        })
        .detach();
    }

    fn reload_config(&mut self) {
        let loaded = config::load(&self.home);
        self.diagnostics.config = loaded
            .error
            .as_ref()
            .map(|error| format!("{}: {error}", loaded.path.display()));
        self.config = loaded.config;
        self.limits_ttl = self.config.refresh.limits_ttl_secs;
        self.refresh_secs = self.config.refresh.ui_secs;
        if self.local_instance_id.is_none() {
            match identity::load_or_create(&self.home) {
                Ok(instance_id) => {
                    self.local_instance_id = Some(instance_id);
                    self.diagnostics.identity = None;
                }
                Err(error) => {
                    self.diagnostics.identity =
                        Some(format!("installation identity unavailable: {error}"));
                }
            }
        }
        if let SourceFilter::Remote(id) = &self.source_filter {
            if !self
                .config
                .ssh_sources
                .iter()
                .any(|source| source.id == *id)
            {
                self.source_filter = SourceFilter::All;
            }
        }
        self.rebuild_from_cache();
    }

    fn rebuild_from_cache(&mut self) {
        let dataset = Dataset::new(
            &self.cache,
            &self.config,
            self.local_instance_id.as_deref(),
            &self.remote_store,
            now_epoch(),
        );
        if let Ok(snapshot) = build_snapshot(
            &dataset,
            &self.source_filter,
            &self.pricing,
            self.period,
            self.round_agent_idx,
            self.diagnostics.clone(),
        ) {
            self.snapshot = snapshot;
        }
    }

    /// Pure key handling (unit-tested).
    fn handle_key(&mut self, key: DashboardKey) {
        match key {
            DashboardKey::TabNext => self.tab = self.tab.next(),
            DashboardKey::TabPrev => self.tab = self.tab.prev(),
            DashboardKey::Left => match self.tab {
                Tab::Rounds => {
                    self.round_agent_idx = self.round_agent_idx.saturating_sub(1);
                    self.rebuild_from_cache();
                }
                _ => {
                    let all = Timeframe::ALL;
                    if let Some(i) = all.iter().position(|t| *t == self.period) {
                        if i > 0 {
                            self.period = all[i - 1];
                            self.rebuild_from_cache();
                        }
                    }
                }
            },
            DashboardKey::Right => match self.tab {
                Tab::Rounds => {
                    self.round_agent_idx =
                        (self.round_agent_idx + 1).min(agg::ROUND_AGENTS.len() - 1);
                    self.rebuild_from_cache();
                }
                _ => {
                    let all = Timeframe::ALL;
                    if let Some(i) = all.iter().position(|t| *t == self.period) {
                        if i + 1 < all.len() {
                            self.period = all[i + 1];
                            self.rebuild_from_cache();
                        }
                    }
                }
            },
            DashboardKey::SourceNext => {
                self.source_filter =
                    cycle_source_filter(&self.source_filter, &self.snapshot.sources, true);
                self.rebuild_from_cache();
            }
            DashboardKey::SourcePrev => {
                self.source_filter =
                    cycle_source_filter(&self.source_filter, &self.snapshot.sources, false);
                self.rebuild_from_cache();
            }
            DashboardKey::Refresh => {
                self.force_refresh = true;
            }
        }
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn set_source_filter(&mut self, filter: SourceFilter, cx: &mut Context<Self>) {
        self.source_filter = filter;
        self.rebuild_from_cache();
        cx.notify();
    }

    fn set_period(&mut self, tf: Timeframe, cx: &mut Context<Self>) {
        self.period = tf;
        self.rebuild_from_cache();
        cx.notify();
    }

    fn set_round_agent(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.round_agent_idx = idx.min(agg::ROUND_AGENTS.len() - 1);
        self.rebuild_from_cache();
        cx.notify();
    }
}

impl Focusable for Dashboard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Dashboard {
    fn render_source_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filters = source_filter_options(&self.snapshot.sources);
        let active = self.source_filter.clone();
        div()
            .id("source-filter-scroll")
            .flex()
            .items_center()
            .gap_1()
            .w_full()
            .overflow_x_scroll()
            .child(
                div()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .mr_1()
                    .child("source"),
            )
            .children(filters.into_iter().map(|filter| {
                let selected = filter == active;
                let label = match &filter {
                    SourceFilter::All => "all sources".to_string(),
                    SourceFilter::Local => self
                        .snapshot
                        .sources
                        .iter()
                        .find(|source| source.local)
                        .map(|source| source.label.clone())
                        .unwrap_or_else(|| "local".to_string()),
                    SourceFilter::Remote(id) => self
                        .snapshot
                        .sources
                        .iter()
                        .find(|source| source.id == *id)
                        .map(|source| source.label.clone())
                        .unwrap_or_else(|| id.clone()),
                };
                let id = match &filter {
                    SourceFilter::All => "all".to_string(),
                    SourceFilter::Local => "local".to_string(),
                    SourceFilter::Remote(id) => id.clone(),
                };
                div()
                    .id(SharedString::from(format!("source-{id}")))
                    .cursor_pointer()
                    .px_1p5()
                    .py_0p5()
                    .rounded(px(3.0))
                    .font_family(MONO)
                    .text_xs()
                    .when(selected, |element| {
                        element
                            .bg(accent())
                            .text_color(pill_fg())
                            .font_weight(gpui::FontWeight::BOLD)
                    })
                    .when(!selected, |element| element.text_color(muted()))
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.set_source_filter(filter.clone(), cx)
                        }),
                    )
                    .child(label)
            }))
    }

    fn render_source_status(&self) -> impl IntoElement {
        let now = now_epoch();
        let diagnostics = [
            self.snapshot.diagnostics.config.as_deref(),
            self.snapshot.diagnostics.identity.as_deref(),
            self.snapshot.diagnostics.remote_store.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .w_full()
            .child(
                div()
                    .id("source-status-scroll")
                    .flex()
                    .items_center()
                    .gap_3()
                    .w_full()
                    .overflow_x_scroll()
                    .children(self.snapshot.sources.iter().map(|source| {
                        let color = source_health_color(source.health);
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .font_family(MONO)
                            .text_xs()
                            .child(
                                div()
                                    .text_color(color)
                                    .child(source_health_icon(source.health)),
                            )
                            .child(div().text_color(text()).child(source.label.clone()))
                            .child(
                                div()
                                    .text_color(color)
                                    .child(source_health_text(source, now)),
                            )
                    })),
            )
            .when(!diagnostics.is_empty(), |element| {
                element.child(
                    div()
                        .font_family(MONO)
                        .text_xs()
                        .text_color(cost_hi())
                        .child(diagnostics),
                )
            })
    }

    fn render_limits(&self) -> impl IntoElement {
        // Name column fixed (longest label + pad); bars share remaining width equally.
        const BAR_H: f32 = 7.0;
        const NAME_CHAR: f32 = 7.5;
        const NAME_PAD: f32 = 12.0;
        let rows = &self.snapshot.limits;
        let show_source = source_identity_visible(&self.source_filter);
        let name_w = rows
            .iter()
            .map(|r| {
                if show_source {
                    r.source_label.chars().count() + r.agent.chars().count() + 3
                } else {
                    r.agent.chars().count()
                }
            })
            .max()
            .unwrap_or(6) as f32
            * NAME_CHAR
            + NAME_PAD;

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .font_family(MONO)
            .children(if rows.is_empty() {
                vec![div()
                    .text_xs()
                    .text_color(dim())
                    .child("limits —")
                    .into_any_element()]
            } else {
                rows.iter()
                    .map(|r| {
                        let name = if show_source {
                            format!("{} · {}", r.source_label, r.agent)
                        } else {
                            r.agent.clone()
                        };
                        let name_color = agent_name_color(&r.agent);
                        let bars: Vec<AnyElement> = if r.windows.is_empty() {
                            vec![div()
                                .flex_1()
                                .text_xs()
                                .text_color(dim())
                                .child("—")
                                .into_any_element()]
                        } else {
                            r.windows
                                .iter()
                                .map(|w| {
                                    let pct = w.pct.clamp(0.0, 100.0);
                                    let fill = usage_fill_color(pct);
                                    // relative fill of full-width track
                                    let frac = (pct as f32 / 100.0).clamp(0.0, 1.0);
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w(px(48.))
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .flex()
                                                .items_baseline()
                                                .gap_1()
                                                .text_xs()
                                                .child(
                                                    div().text_color(dim()).child(w.label.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_color(fill)
                                                        .child(format!("{pct:.0}%")),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .h(px(BAR_H))
                                                .rounded(px(3.0))
                                                .bg(rgb(0x2a2a32))
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .h_full()
                                                        .rounded(px(3.0))
                                                        .bg(fill)
                                                        .w(relative(frac)),
                                                ),
                                        )
                                        .into_any_element()
                                })
                                .collect()
                        };
                        div()
                            .flex()
                            .items_end()
                            .gap_2()
                            .w_full()
                            .child(
                                div()
                                    .w(px(name_w))
                                    .flex_shrink_0()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(name_color)
                                    .child(name),
                            )
                            .child(div().flex().flex_1().gap_2().min_w(px(0.)).children(bars))
                            .into_any_element()
                    })
                    .collect()
            })
    }

    fn render_tabbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tab;
        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .children(Tab::ALL.into_iter().map(|tab| {
                let is_active = tab == active;
                let label = if is_active {
                    format!("▸{}◂", tab.label())
                } else {
                    tab.label().to_string()
                };
                div()
                    .id(SharedString::from(format!("tab-{}", tab.label())))
                    .cursor_pointer()
                    .px_1()
                    .py_0p5()
                    .font_family(MONO)
                    .text_sm()
                    .font_weight(if is_active {
                        gpui::FontWeight::BOLD
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .text_color(if is_active { accent() } else { muted() })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(label)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .font_family(MONO)
                    .text_sm()
                    .text_color(muted())
                    .child(self.snapshot.clock.clone()),
            )
    }

    fn render_period(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.period;
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .child("period"),
            )
            .children(Timeframe::ALL.into_iter().map(|p| {
                let is_active = p == active;
                div()
                    .id(SharedString::from(format!("period-{}", p.label())))
                    .cursor_pointer()
                    .px_1p5()
                    .py_0p5()
                    .rounded(px(3.0))
                    .font_family(MONO)
                    .text_xs()
                    .when(is_active, |s| {
                        s.bg(accent())
                            .text_color(pill_fg())
                            .font_weight(gpui::FontWeight::BOLD)
                    })
                    .when(!is_active, |s| s.text_color(muted()))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_period(p, cx)))
                    .child(p.label())
            }))
    }

    fn render_chart(&self) -> impl IntoElement {
        let values: Vec<u64> = self.snapshot.chart.iter().map(|b| b.tokens).collect();
        let peak = values.iter().copied().max().unwrap_or(0);
        let left = self
            .snapshot
            .chart
            .first()
            .map(|b| b.label.as_str())
            .unwrap_or("")
            .to_string();
        let right = self
            .snapshot
            .chart
            .last()
            .map(|b| b.label.as_str())
            .unwrap_or("")
            .to_string();
        let unit = match self.period {
            Timeframe::Day => "hour",
            Timeframe::Week | Timeframe::Month => "day",
            Timeframe::Quarter => "week",
            Timeframe::All => "month",
        };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(
                div()
                    .flex()
                    .w_full()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_xs()
                            .text_color(dim())
                            .child(format!("tokens/{unit}")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(MONO)
                            .text_xs()
                            .text_color(dim())
                            .child(format!("peak {}", ftok(peak))),
                    ),
            )
            .child(
                div().w_full().h(px(CHART_H)).child(
                    canvas(
                        move |_, _, _| (),
                        move |bounds, _, window, _| paint_bars(bounds, &values, peak, window),
                    )
                    .size_full(),
                ),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_xs()
                            .text_color(dim())
                            .child(left),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(MONO)
                            .text_xs()
                            .text_color(dim())
                            .child(right),
                    ),
            )
    }

    fn render_metrics(&self) -> impl IntoElement {
        // A left | B right | C left | D left
        // Σ      4.7B   $4078.52
        // rate   4.1M   this hr     28.1M/h avg
        // rounds 2064   turns       2.3M/turn
        const A: f32 = 64.0;
        const B: f32 = 72.0;
        const C: f32 = 96.0;
        const D: f32 = 104.0;

        let s = &self.snapshot;
        let row = |a: SharedString,
                   a_color: gpui::Rgba,
                   b: SharedString,
                   b_color: gpui::Rgba,
                   c: SharedString,
                   c_color: gpui::Rgba,
                   d: SharedString,
                   d_color: gpui::Rgba,
                   bold: bool| {
            div()
                .flex()
                .w_full()
                .items_baseline()
                .font_family(MONO)
                .text_sm()
                .font_weight(if bold {
                    gpui::FontWeight::BOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .child(div().w(px(A)).text_color(a_color).child(a))
                .child(div().w(px(B)).text_right().text_color(b_color).child(b))
                .child(div().w(px(C)).pl_3().text_color(c_color).child(c))
                .child(div().w(px(D)).pl_2().text_color(d_color).child(d))
        };

        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .w_full()
            .child(row(
                "Σ".into(),
                accent(),
                ftok(s.total_tokens).into(),
                text(),
                fcost(s.total_cost).into(),
                cost_color(s.total_cost, s.tf),
                "".into(),
                muted(),
                true,
            ))
            .child(row(
                "rate".into(),
                dim(),
                ftok(s.rate_hour).into(),
                text(),
                "this hr".into(),
                muted(),
                format!("{}/h avg", ftok(s.per_h as u64)).into(),
                text(),
                false,
            ))
            .when(s.rounds_known, |d| {
                d.child(row(
                    "rounds".into(),
                    dim(),
                    format!("{}", s.rounds_total).into(),
                    text(),
                    "turns".into(),
                    muted(),
                    format!("{}/turn", ftok(s.per_round as u64)).into(),
                    text(),
                    false,
                ))
            })
    }

    fn render_agents(&self) -> impl IntoElement {
        let s = &self.snapshot;
        let total_req: u64 = s.agents.iter().map(|a| a.req).sum();
        let total_in: u64 = s.agents.iter().map(|a| a.input).sum();
        let total_out: u64 = s.agents.iter().map(|a| a.output).sum();
        let total_cache: u64 = s.agents.iter().map(|a| a.cache).sum();
        let total_cost: f64 = s.agents.iter().map(|a| a.cost).sum();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(section_header("BY AGENT", s.tf.label()))
            .child(agent_header_row())
            .children(s.agents.iter().map(|a| {
                agent_row(
                    &a.name, a.req, a.input, a.output, a.cache, a.cost, false, s.tf,
                )
            }))
            .child(agent_row(
                "─ total",
                total_req,
                total_in,
                total_out,
                total_cache,
                total_cost,
                true,
                s.tf,
            ))
    }

    fn render_models(&self) -> impl IntoElement {
        let s = &self.snapshot;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(section_header("BY MODEL", s.tf.label()))
            .child(agent_header_row())
            .children(s.models.iter().map(|m| {
                agent_row(
                    &m.name, m.req, m.input, m.output, m.cache, m.cost, false, s.tf,
                )
            }))
    }

    fn render_projects_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = &self.snapshot.projects_tab;
        let total = &self.snapshot.projects_total;
        let show_source = source_identity_visible(&self.source_filter);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .child(self.render_period(cx))
            .child(section_header("TOP PROJECTS", self.snapshot.tf.label()))
            .children(rows.iter().map(|p| {
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_sm()
                    .when(show_source, |row| {
                        row.child(
                            div()
                                .w(px(88.))
                                .flex_shrink_0()
                                .text_color(muted())
                                .overflow_hidden()
                                .child(p.source_label.clone()),
                        )
                    })
                    .child(div().flex_1().child(p.name.clone()))
                    .child(div().w(px(64.)).text_right().child(ftok(p.tokens)))
                    .child(
                        div()
                            .w(px(80.))
                            .text_right()
                            .text_color(cost_color(p.cost, self.snapshot.tf))
                            .child(fcost(p.cost)),
                    )
            }))
            .child(
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_sm()
                    .font_weight(gpui::FontWeight::BOLD)
                    .when(show_source, |row| {
                        row.child(div().w(px(88.)).flex_shrink_0())
                    })
                    .child(div().flex_1().child("─ all"))
                    .child(div().w(px(64.)).text_right().child(ftok(total.tokens())))
                    .child(
                        div()
                            .w(px(80.))
                            .text_right()
                            .text_color(cost_color(total.cost, self.snapshot.tf))
                            .child(fcost(total.cost)),
                    ),
            )
    }

    fn render_rounds_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sel = self.round_agent_idx;
        let show_source = source_identity_visible(&self.source_filter);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_xs()
                            .text_color(dim())
                            .child("agent"),
                    )
                    .children(agg::ROUND_AGENTS.iter().enumerate().map(|(i, name)| {
                        let active = i == sel;
                        div()
                            .id(SharedString::from(format!("round-agent-{name}")))
                            .cursor_pointer()
                            .px_1p5()
                            .py_0p5()
                            .rounded(px(3.0))
                            .font_family(MONO)
                            .text_xs()
                            .when(active, |s| {
                                s.bg(accent())
                                    .text_color(pill_fg())
                                    .font_weight(gpui::FontWeight::BOLD)
                            })
                            .when(!active, |s| s.text_color(muted()))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_round_agent(i, cx)),
                            )
                            .child(*name)
                    })),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .child(div().w(px(48.)).child("time"))
                    .when(show_source, |row| {
                        row.child(div().w(px(72.)).child("source"))
                    })
                    .child(div().w(px(56.)).child("agent"))
                    .child(div().w(px(120.)).child("model"))
                    .child(div().flex_1().child("project"))
                    .child(div().w(px(56.)).text_right().child("tok"))
                    .child(div().w(px(64.)).text_right().child("$")),
            )
            .children(self.snapshot.rounds.iter().map(|r| {
                let ac = agent_name_color(&r.agent);
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_sm()
                    .child(div().w(px(48.)).text_color(dim()).child(r.time.clone()))
                    .when(show_source, |row| {
                        row.child(
                            div()
                                .w(px(72.))
                                .text_color(muted())
                                .overflow_hidden()
                                .child(r.source_label.clone()),
                        )
                    })
                    .child(div().w(px(56.)).text_color(ac).child(r.agent.clone()))
                    .child(
                        div()
                            .w(px(120.))
                            .text_color(muted())
                            .overflow_hidden()
                            .child(r.model.clone()),
                    )
                    .child(div().flex_1().text_color(text()).child(r.project.clone()))
                    .child(div().w(px(56.)).text_right().child(ftok(r.tokens)))
                    .child(
                        div()
                            .w(px(64.))
                            .text_right()
                            .text_color(cost_color(r.cost, Timeframe::Day))
                            .child(fcost(r.cost)),
                    )
            }))
    }
}

fn section_header(title: impl Into<SharedString>, tf: impl Into<SharedString>) -> impl IntoElement {
    let title = title.into();
    let tf = tf.into();
    div()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .child(
            div()
                .font_family(MONO)
                .text_xs()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(text())
                .child(title),
        )
        .child(div().flex_1().h(px(1.)).bg(rgb(0x2e2e36)))
        .child(
            div()
                .font_family(MONO)
                .text_xs()
                .text_color(dim())
                .child(tf),
        )
}

fn agent_header_row() -> impl IntoElement {
    // Name flexes; metric columns share the right edge like BY MODEL.
    div()
        .flex()
        .w_full()
        .font_family(MONO)
        .text_xs()
        .text_color(dim())
        .child(div().flex_1().child(""))
        .child(div().w(px(56.)).text_right().child("req"))
        .child(div().w(px(64.)).text_right().child("in"))
        .child(div().w(px(64.)).text_right().child("out"))
        .child(div().w(px(64.)).text_right().child("cache"))
        .child(div().w(px(80.)).text_right().child("$"))
}

#[allow(clippy::too_many_arguments)]
fn agent_row(
    name: &str,
    req: u64,
    input: u64,
    output: u64,
    cache: u64,
    cost: f64,
    bold: bool,
    tf: Timeframe,
) -> impl IntoElement {
    let name_s: SharedString = name.to_string().into();
    let name_color = agent_name_color(name);
    div()
        .flex()
        .w_full()
        .font_family(MONO)
        .text_sm()
        .font_weight(if bold {
            gpui::FontWeight::BOLD
        } else {
            gpui::FontWeight::NORMAL
        })
        .child(div().flex_1().text_color(name_color).child(name_s))
        .child(
            div()
                .w(px(56.))
                .text_right()
                .text_color(text())
                .child(format!("{req}")),
        )
        .child(
            div()
                .w(px(64.))
                .text_right()
                .text_color(text())
                .child(ftok(input)),
        )
        .child(
            div()
                .w(px(64.))
                .text_right()
                .text_color(text())
                .child(ftok(output)),
        )
        .child(
            div()
                .w(px(64.))
                .text_right()
                .text_color(text())
                .child(ftok(cache)),
        )
        .child(
            div()
                .w(px(80.))
                .text_right()
                .text_color(cost_color(cost, tf))
                .child(fcost(cost)),
        )
}

fn paint_bars(bounds: Bounds<Pixels>, values: &[u64], peak: u64, window: &mut Window) {
    if values.is_empty() || peak == 0 {
        return;
    }
    let n = values.len() as f32;
    let gap = px(3.0);
    let total_gap = gap * (n - 1.0).max(0.0);
    let bar_w = ((bounds.size.width - total_gap) / n).max(px(4.0));
    let max_h = bounds.size.height;
    for (i, &v) in values.iter().enumerate() {
        let level = v as f32 / peak as f32;
        let h = max_h * level;
        let x = bounds.origin.x + (bar_w + gap) * i as f32;
        let y = bounds.origin.y + max_h - h;
        window.paint_quad(fill(
            Bounds {
                origin: point(x, y),
                size: size(bar_w, h),
            },
            bar_color(level),
        ));
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Ensure focus so keybindings work.
        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }

        div()
            .id("dashboard")
            .track_focus(&self.focus_handle)
            .key_context("Dashboard")
            .size_full()
            .bg(bg())
            .text_color(text())
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .on_action(cx.listener(|this, _: &NextTab, _, cx| {
                this.handle_key(DashboardKey::TabNext);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| {
                this.handle_key(DashboardKey::TabPrev);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PeriodLeft, _, cx| {
                this.handle_key(DashboardKey::Left);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PeriodRight, _, cx| {
                this.handle_key(DashboardKey::Right);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SourceNext, _, cx| {
                this.handle_key(DashboardKey::SourceNext);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SourcePrev, _, cx| {
                this.handle_key(DashboardKey::SourcePrev);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ForceRefresh, _, cx| {
                this.handle_key(DashboardKey::Refresh);
                this.remote_refresh.request_force();
                this.reload_config();
                this.schedule_reload(cx, false);
                this.schedule_remote_refresh(cx);
                cx.notify();
            }))
            .child(self.render_source_filter(cx))
            .child(self.render_source_status())
            .child(div().h(px(1.)).w_full().bg(rgb(0x2a2a30)))
            .child(self.render_limits())
            .child(div().h(px(1.)).w_full().bg(rgb(0x2a2a30)))
            .child(self.render_tabbar(cx))
            .child(div().h(px(1.)).w_full().bg(rgb(0x2a2a30)))
            .when_some(self.status.clone(), |d, st| {
                d.child(
                    div()
                        .font_family(MONO)
                        .text_sm()
                        .text_color(accent())
                        .child(st),
                )
            })
            .child(match self.tab {
                Tab::Global => div()
                    .id("global-scroll")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .flex_1()
                    .overflow_y_scroll()
                    .child(self.render_period(cx))
                    .child(self.render_chart())
                    .child(self.render_metrics())
                    .child(self.render_agents())
                    .child(self.render_models())
                    .into_any_element(),
                Tab::Projects => div()
                    .id("projects-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(self.render_projects_tab(cx))
                    .into_any_element(),
                Tab::Rounds => div()
                    .id("rounds-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(self.render_rounds_tab(cx))
                    .into_any_element(),
            })
            .child(
                div()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .child("Tab tabs · ←→ period/agent · s source · r refresh"),
            )
    }
}

// ── CLI / main ──────────────────────────────────────────────────────────────

fn build_export_snapshot(
    config: &Config,
    instance_id: String,
    outcome: LocalRefreshOutcome,
    now: i64,
) -> SourceSnapshot {
    SourceSnapshot {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        cache_version: CACHE_VERSION,
        instance_id,
        generated_at: now,
        utc_offset_secs: local_offset(now),
        refresh_status: outcome.refresh_status,
        retention: ProtocolRetention {
            history_days: config.retention.history_days,
            hours_days: config.retention.hours_days,
        },
        data: outcome.cache.compact_data(),
    }
}

fn run_export_source() -> Result<(), String> {
    let home = env::var("HOME").unwrap_or_default();
    let loaded = config::load(&home);
    if let Some(error) = &loaded.error {
        eprintln!("tokmeter: {}: {error}", loaded.path.display());
    }
    let instance_id = identity::load_or_create(&home)
        .map_err(|error| format!("installation identity unavailable: {error}"))?;
    let outcome = engine::reload_configured(
        &home,
        &loaded.config,
        loaded.config.refresh.limits_ttl_secs,
        false,
    );
    let snapshot = build_export_snapshot(&loaded.config, instance_id, outcome, now_epoch());
    let json = snapshot
        .to_json()
        .map_err(|error| format!("snapshot encoding failed: {error}"))?;
    println!("{json}");
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DumpMode {
    Global,
    Projects,
    Rounds,
    Limits,
    Sources,
}

impl DumpMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "global" => Ok(Self::Global),
            "projects" => Ok(Self::Projects),
            "rounds" => Ok(Self::Rounds),
            "limits" => Ok(Self::Limits),
            "sources" => Ok(Self::Sources),
            _ => Err(format!("unknown dump mode {value}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Projects => "projects",
            Self::Rounds => "rounds",
            Self::Limits => "limits",
            Self::Sources => "sources",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DumpArgs {
    mode: DumpMode,
    source: SourceFilter,
}

fn dump_args_from_args(args: &[String]) -> Result<Option<DumpArgs>, String> {
    let mut mode = None;
    let mut source = None;
    for arg in args.iter().skip(1) {
        let parsed_mode = if arg == "--dump-json" {
            Some(DumpMode::Global)
        } else if let Some(value) = arg.strip_prefix("--dump-json=") {
            Some(DumpMode::parse(value)?)
        } else {
            None
        };
        if let Some(parsed_mode) = parsed_mode {
            if mode.replace(parsed_mode).is_some() {
                return Err("dump mode specified more than once".to_string());
            }
        }
        if let Some(value) = arg.strip_prefix("--source=") {
            if value.is_empty() {
                return Err("source id cannot be empty".to_string());
            }
            if source.replace(SourceFilter::parse(value)).is_some() {
                return Err("source specified more than once".to_string());
            }
        }
    }
    Ok(mode.map(|mode| DumpArgs {
        mode,
        source: source.unwrap_or(SourceFilter::All),
    }))
}

fn validate_dump_source(config: &Config, filter: &SourceFilter) -> Result<(), String> {
    if let SourceFilter::Remote(id) = filter {
        if !config.ssh_sources.iter().any(|source| source.id == *id) {
            return Err(format!("unknown source {id}"));
        }
    }
    Ok(())
}

fn run_dump(args: DumpArgs) -> Result<(), String> {
    let home = env::var("HOME").unwrap_or_default();
    let loaded = config::load(&home);
    validate_dump_source(&loaded.config, &args.source)?;
    let mut diagnostics = DiagnosticsOwned {
        config: loaded
            .error
            .as_ref()
            .map(|error| format!("{}: {error}", loaded.path.display())),
        ..DiagnosticsOwned::default()
    };
    let config = loaded.config;
    let pricing = Pricing::load(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/pricing.json"
    )));
    let limits_ttl = if args.mode == DumpMode::Limits {
        config.refresh.limits_ttl_secs
    } else {
        0
    };
    let outcome = engine::reload_configured(&home, &config, limits_ttl, false);
    let local_instance_id = match identity::load_or_create(&home) {
        Ok(instance_id) => Some(instance_id),
        Err(error) => {
            diagnostics.identity = Some(format!("installation identity unavailable: {error}"));
            None
        }
    };
    let retention = ProtocolRetention {
        history_days: config.retention.history_days,
        hours_days: config.retention.hours_days,
    };
    let mut store = RemoteStore::load(&home, now_epoch(), retention);
    diagnostics.remote_store = store.load_error.clone();
    let enabled_sources = remote_sources_due(
        &config,
        &store,
        now_epoch(),
        true,
        local_instance_id.is_some(),
    );
    let mut coordinator = RemoteCoordinator::default();
    let results = coordinator.start(enabled_sources, config.refresh, retention, &mut store);
    for result in results {
        coordinator.apply(result, &mut store);
    }
    if let Err(error) = store.save() {
        diagnostics.remote_store = Some(format!("remote cache save failed: {error}"));
    }
    let dataset = Dataset::new(
        &outcome.cache,
        &config,
        local_instance_id.as_deref(),
        &store,
        now_epoch(),
    );
    let snapshot = build_snapshot(
        &dataset,
        &args.source,
        &pricing,
        Timeframe::Week,
        0,
        diagnostics,
    )?;
    let value = snapshot_to_json(&snapshot, args.mode.name());
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("dump encoding failed: {error}"))?;
    println!("{text}");
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|arg| arg == "--export-source-json") {
        if let Err(error) = run_export_source() {
            eprintln!("tokmeter: {error}");
            std::process::exit(1);
        }
        return;
    }
    match dump_args_from_args(&args) {
        Ok(Some(dump_args)) => {
            if let Err(error) = run_dump(dump_args) {
                eprintln!("tokmeter: {error}");
                std::process::exit(1);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("tokmeter: {error}");
            std::process::exit(2);
        }
    }

    application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("tab", NextTab, Some("Dashboard")),
            KeyBinding::new("shift-tab", PrevTab, Some("Dashboard")),
            KeyBinding::new("left", PeriodLeft, Some("Dashboard")),
            KeyBinding::new("right", PeriodRight, Some("Dashboard")),
            KeyBinding::new("s", SourceNext, Some("Dashboard")),
            KeyBinding::new("shift-s", SourcePrev, Some("Dashboard")),
            KeyBinding::new("r", ForceRefresh, Some("Dashboard")),
        ]);

        let bounds = Bounds::centered(None, size(px(UI_W), px(UI_H)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(420.), px(480.))),
                app_id: Some("tokmeter".into()),
                window_background: WindowBackgroundAppearance::Opaque,
                focus: true,
                show: true,
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("tokmeter");
                cx.new(|cx| {
                    let dash = Dashboard::new(cx);
                    window.focus(&dash.focus_handle, cx);
                    dash
                })
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod export_source_tests {
    use super::*;
    use data::protocol::{decode_json, ExportRefreshStatus};

    #[test]
    fn builds_local_only_protocol_snapshot() {
        let config = Config::default();
        let cache = Cache::load(std::env::temp_dir().join("tokmeter-export-test-missing.json"));
        let now = 1_784_317_200;
        let snapshot = build_export_snapshot(
            &config,
            "123e4567-e89b-12d3-a456-426614174000".into(),
            LocalRefreshOutcome {
                cache,
                refresh_status: ExportRefreshStatus::ReadOnly,
            },
            now,
        );
        let decoded = decode_json(&snapshot.to_json().unwrap(), now, snapshot.retention).unwrap();
        assert_eq!(decoded.snapshot.instance_id, snapshot.instance_id);
        assert_eq!(
            decoded.snapshot.refresh_status,
            ExportRefreshStatus::ReadOnly
        );
        assert_eq!(
            decoded.snapshot.retention.history_days,
            config.retention.history_days
        );
    }
}

#[cfg(test)]
mod dashboard_refresh_tests {
    use super::*;
    use data::config::SshSourceConfig;

    fn source(id: &str, enabled: bool) -> SshSourceConfig {
        SshSourceConfig {
            id: id.into(),
            label: id.to_uppercase(),
            host: id.into(),
            enabled,
            binary: "tokmeter".into(),
        }
    }

    #[test]
    fn remote_gate_preserves_force_requested_during_flight() {
        let mut state = RemoteRefreshState::default();
        assert!(state.begin(true));
        assert!(state.in_flight);
        state.request_force();
        assert!(!state.begin(true));
        assert!(state.finish());
        assert!(!state.in_flight);
        assert!(state.begin(true));
        assert!(!state.force);
    }

    #[test]
    fn remote_due_respects_enabled_interval_and_force() {
        let config = Config {
            ssh_sources: vec![
                source("due", true),
                source("recent", true),
                source("off", false),
            ],
            ..Config::default()
        };
        let mut store = RemoteStore::empty("/tmp");
        store.set_connecting("due", "DUE", 100);
        store.set_connecting("recent", "RECENT", 190);
        let due = remote_sources_due(&config, &store, 200, false, true);
        assert_eq!(
            due.iter()
                .map(|source| source.id.as_str())
                .collect::<Vec<_>>(),
            vec!["due"]
        );
        assert!(remote_sources_due(&config, &store, 200, true, false).is_empty());
        let forced = remote_sources_due(&config, &store, 200, true, true);
        assert_eq!(
            forced
                .iter()
                .map(|source| source.id.as_str())
                .collect::<Vec<_>>(),
            vec!["due", "recent"]
        );
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use data::config::SshSourceConfig;

    fn args(values: &[&str]) -> Vec<String> {
        std::iter::once("tokmeter")
            .chain(values.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn parses_dump_modes_and_source_filters() {
        assert_eq!(
            dump_args_from_args(&args(&["--dump-json"])).unwrap(),
            Some(DumpArgs {
                mode: DumpMode::Global,
                source: SourceFilter::All,
            })
        );
        assert_eq!(
            dump_args_from_args(&args(&["--dump-json=sources"])).unwrap(),
            Some(DumpArgs {
                mode: DumpMode::Sources,
                source: SourceFilter::All,
            })
        );
        assert_eq!(
            dump_args_from_args(&args(&["--dump-json=projects", "--source=local"])).unwrap(),
            Some(DumpArgs {
                mode: DumpMode::Projects,
                source: SourceFilter::Local,
            })
        );
        assert_eq!(
            dump_args_from_args(&args(&["--dump-json=rounds", "--source=lxc"])).unwrap(),
            Some(DumpArgs {
                mode: DumpMode::Rounds,
                source: SourceFilter::Remote("lxc".into()),
            })
        );
    }

    #[test]
    fn rejects_invalid_or_duplicate_dump_arguments() {
        assert!(dump_args_from_args(&args(&["--dump-json=unknown"])).is_err());
        assert!(dump_args_from_args(&args(&["--dump-json", "--dump-json=limits"])).is_err());
        assert!(dump_args_from_args(&args(&["--dump-json", "--source="])).is_err());
        assert!(
            dump_args_from_args(&args(&["--dump-json", "--source=local", "--source=lxc"])).is_err()
        );
        assert_eq!(
            dump_args_from_args(&args(&["--source=local"])).unwrap(),
            None
        );
    }

    #[test]
    fn validates_configured_sources_including_disabled_ones() {
        let config = Config {
            ssh_sources: vec![SshSourceConfig {
                id: "lxc".into(),
                label: "LXC".into(),
                host: "lxc".into(),
                enabled: false,
                binary: "tokmeter".into(),
            }],
            ..Config::default()
        };
        assert!(validate_dump_source(&config, &SourceFilter::Local).is_ok());
        assert!(validate_dump_source(&config, &SourceFilter::Remote("lxc".into())).is_ok());
        assert!(validate_dump_source(&config, &SourceFilter::Remote("missing".into())).is_err());
    }
}

#[cfg(test)]
mod json_schema_tests {
    use super::*;
    use data::cache::{agg_key, CompactData};
    use data::config::SshSourceConfig;
    use data::limits::{Snapshot, Window};
    use data::protocol::ExportRefreshStatus;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn snapshot() -> ViewSnapshot {
        let mut snapshot = ViewSnapshot {
            total_tokens: 30,
            total_cost: 1.5,
            projects_total: Tot {
                req: 1,
                inp: 30,
                out: 0,
                cache: 0,
                cost: 1.5,
            },
            ..ViewSnapshot::default()
        };
        snapshot.agents.push(AgentRowOwned {
            name: "claude".into(),
            req: 1,
            input: 30,
            output: 0,
            cache: 0,
            cost: 1.5,
        });
        snapshot.models.push(ModelRowOwned {
            name: "opus".into(),
            req: 1,
            input: 30,
            output: 0,
            cache: 0,
            tokens: 30,
            cost: 1.5,
        });
        snapshot.projects_tab.push(ProjectRowOwned {
            source_id: "lxc".into(),
            source_label: "LXC".into(),
            path: "/srv/project".into(),
            name: "srv/project".into(),
            tokens: 30,
            cost: 1.5,
        });
        snapshot.rounds.push(RoundRowOwned {
            source_id: "lxc".into(),
            source_label: "LXC".into(),
            time: "12:00".into(),
            agent: "claude".into(),
            model: "opus".into(),
            project: "srv/project".into(),
            tokens: 30,
            cost: 1.5,
        });
        snapshot.limits.push(LimitRowOwned {
            source_id: "lxc".into(),
            source_label: "LXC".into(),
            agent: "claude".into(),
            windows: vec![LimitWinOwned {
                label: "5h".into(),
                pct: 42.0,
            }],
            age: 5,
        });
        snapshot.sources.push(SourceOwned {
            id: "lxc".into(),
            label: "LXC".into(),
            local: false,
            enabled: true,
            active: true,
            health: SourceHealth::Stale,
            warnings: vec![SourceWarning::PartialHistory],
            last_attempt: 20,
            last_success: 10,
            duration_ms: 15,
            error: "offline".into(),
        });
        snapshot.diagnostics.config = Some("invalid config".into());
        snapshot
    }

    #[test]
    fn global_json_keeps_existing_fields_and_adds_sources() {
        let value = snapshot_to_json(&snapshot(), "global");
        for field in [
            "tf",
            "total_tokens",
            "total_cost",
            "agents",
            "models",
            "projects",
            "rounds",
            "limits",
        ] {
            assert!(value.get(field).is_some(), "{field}");
        }
        assert_eq!(value["projects"][0]["source_id"], "lxc");
        assert_eq!(value["projects"][0]["path"], "/srv/project");
        assert_eq!(value["rounds"][0]["source_id"], "lxc");
        assert_eq!(value["limits"][0]["source_id"], "lxc");
        assert_eq!(value["sources"][0]["health"], "stale");
        assert!(value["sources"][0].get("instance_id").is_none());
        assert_eq!(value["sources"][0]["warnings"][0], "partial_history");
        assert_eq!(value["diagnostics"]["config"], "invalid config");
    }

    #[test]
    fn mode_json_keeps_source_identity_and_statuses() {
        let snapshot = snapshot();
        assert_eq!(
            snapshot_to_json(&snapshot, "projects")["projects"][0]["source_id"],
            "lxc"
        );
        assert_eq!(
            snapshot_to_json(&snapshot, "rounds")["rounds"][0]["source_id"],
            "lxc"
        );
        assert_eq!(
            snapshot_to_json(&snapshot, "limits")["limits"][0]["source_id"],
            "lxc"
        );
        let sources = snapshot_to_json(&snapshot, "sources");
        assert!(sources.get("tf").is_none());
        assert_eq!(sources["sources"][0]["id"], "lxc");
        assert!(sources.get("diagnostics").is_some());
    }

    #[test]
    fn view_snapshot_applies_source_filter_and_keeps_limits_separate() {
        let now = now_epoch();
        let off = local_offset(now);
        let date = data::timeutil::ymd_str(local_day(now, off));
        let key = agg_key(&date, "claude", "opus", "standard", "/same/project");
        let mut local = Cache::load(PathBuf::from("/does/not/exist"));
        local.agg.insert(key.clone(), [1, 10, 0, 0, 0, 0]);
        local.limits.insert(
            "grok".into(),
            Snapshot {
                ts: now,
                checked: now,
                windows: vec![Window {
                    label: "wk".into(),
                    pct: 50.0,
                    resets: 0,
                }],
            },
        );
        let mut remote_data = CompactData::default();
        remote_data.agg.insert(key, [1, 20, 0, 0, 0, 0]);
        remote_data.limits.insert(
            "claude".into(),
            Snapshot {
                ts: now,
                checked: now,
                windows: vec![Window {
                    label: "5h".into(),
                    pct: 42.0,
                    resets: 0,
                }],
            },
        );
        let config = Config {
            ssh_sources: vec![SshSourceConfig {
                id: "lxc".into(),
                label: "LXC".into(),
                host: "lxc".into(),
                enabled: true,
                binary: "tokmeter".into(),
            }],
            ..Config::default()
        };
        let mut store = RemoteStore::empty("/tmp");
        store.apply_success(
            "lxc",
            "LXC",
            SourceSnapshot {
                app_version: "0.1.8".into(),
                cache_version: CACHE_VERSION,
                instance_id: "223e4567-e89b-12d3-a456-426614174000".into(),
                generated_at: now,
                utc_offset_secs: off,
                refresh_status: ExportRefreshStatus::Fresh,
                retention: ProtocolRetention {
                    history_days: 120,
                    hours_days: 8,
                },
                data: remote_data,
            },
            Vec::new(),
            now,
            1,
        );
        let pricing = Pricing::load("{}");
        let dataset = Dataset::new(
            &local,
            &config,
            Some("123e4567-e89b-12d3-a456-426614174000"),
            &store,
            now,
        );
        let all = build_snapshot(
            &dataset,
            &SourceFilter::All,
            &pricing,
            Timeframe::Week,
            0,
            DiagnosticsOwned::default(),
        )
        .unwrap();
        let local_only = build_snapshot(
            &dataset,
            &SourceFilter::Local,
            &pricing,
            Timeframe::Week,
            0,
            DiagnosticsOwned::default(),
        )
        .unwrap();
        let remote_only = build_snapshot(
            &dataset,
            &SourceFilter::Remote("lxc".into()),
            &pricing,
            Timeframe::Week,
            0,
            DiagnosticsOwned::default(),
        )
        .unwrap();
        assert_eq!(all.total_tokens, 30);
        assert_eq!(local_only.total_tokens, 10);
        assert_eq!(remote_only.total_tokens, 20);
        assert_eq!(all.projects_tab.len(), 2);
        assert_eq!(
            all.projects_tab
                .iter()
                .map(|project| project.source_id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["local", "lxc"])
        );
        assert!(all.limits.iter().all(|row| row.agent != "grok"));
        assert_eq!(
            all.limits
                .iter()
                .map(|row| row.source_id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["local", "lxc"])
        );
        assert_eq!(all.sources.len(), 2);
    }
}

#[cfg(test)]
mod source_filter_tests {
    use super::*;

    fn source(id: &str, local: bool) -> SourceOwned {
        SourceOwned {
            id: id.into(),
            label: id.to_uppercase(),
            local,
            enabled: true,
            active: true,
            health: SourceHealth::Healthy,
            warnings: Vec::new(),
            last_attempt: 0,
            last_success: 0,
            duration_ms: 0,
            error: String::new(),
        }
    }

    #[test]
    fn cycles_all_local_and_remote_sources_in_both_directions() {
        let sources = vec![
            source("local", true),
            source("one", false),
            source("two", false),
        ];
        let mut filter = SourceFilter::All;
        filter = cycle_source_filter(&filter, &sources, true);
        assert_eq!(filter, SourceFilter::Local);
        filter = cycle_source_filter(&filter, &sources, true);
        assert_eq!(filter, SourceFilter::Remote("one".into()));
        filter = cycle_source_filter(&filter, &sources, true);
        assert_eq!(filter, SourceFilter::Remote("two".into()));
        filter = cycle_source_filter(&filter, &sources, true);
        assert_eq!(filter, SourceFilter::All);
        filter = cycle_source_filter(&filter, &sources, false);
        assert_eq!(filter, SourceFilter::Remote("two".into()));
    }
}

#[cfg(test)]
mod source_status_tests {
    use super::*;

    #[test]
    fn status_text_combines_icon_age_warning_and_error() {
        let source = SourceOwned {
            id: "lxc".into(),
            label: "LXC".into(),
            local: false,
            enabled: true,
            active: true,
            health: SourceHealth::Stale,
            warnings: vec![SourceWarning::PartialHistory],
            last_attempt: 200,
            last_success: 100,
            duration_ms: 10,
            error: "offline".into(),
        };
        assert_eq!(source_health_icon(source.health), "◐");
        let text = source_health_text(&source, 220);
        assert!(text.contains("stale"));
        assert!(text.contains("2m old"));
        assert!(text.contains("partial history"));
        assert!(text.contains("offline"));
    }
}

#[cfg(test)]
mod source_aware_view_tests {
    use super::*;

    #[test]
    fn source_identity_is_shown_only_for_combined_rows() {
        assert!(source_identity_visible(&SourceFilter::All));
        assert!(!source_identity_visible(&SourceFilter::Local));
        assert!(!source_identity_visible(&SourceFilter::Remote(
            "lxc".into()
        )));
    }
}

// ── handle_key unit tests ───────────────────────────────────────────────────

#[cfg(test)]
mod handle_key_tests {
    use super::*;

    struct TestDash {
        tab: Tab,
        period: Timeframe,
        round_agent_idx: usize,
        force_refresh: bool,
    }

    impl TestDash {
        fn handle_key(&mut self, key: DashboardKey) {
            match key {
                DashboardKey::TabNext => self.tab = self.tab.next(),
                DashboardKey::TabPrev => self.tab = self.tab.prev(),
                DashboardKey::Left => match self.tab {
                    Tab::Rounds => {
                        self.round_agent_idx = self.round_agent_idx.saturating_sub(1);
                    }
                    _ => {
                        let all = Timeframe::ALL;
                        if let Some(i) = all.iter().position(|t| *t == self.period) {
                            if i > 0 {
                                self.period = all[i - 1];
                            }
                        }
                    }
                },
                DashboardKey::Right => match self.tab {
                    Tab::Rounds => {
                        self.round_agent_idx =
                            (self.round_agent_idx + 1).min(agg::ROUND_AGENTS.len() - 1);
                    }
                    _ => {
                        let all = Timeframe::ALL;
                        if let Some(i) = all.iter().position(|t| *t == self.period) {
                            if i + 1 < all.len() {
                                self.period = all[i + 1];
                            }
                        }
                    }
                },
                DashboardKey::SourceNext | DashboardKey::SourcePrev => {}
                DashboardKey::Refresh => self.force_refresh = true,
            }
        }
    }

    #[test]
    fn handle_key_tab_cycles() {
        let mut d = TestDash {
            tab: Tab::Global,
            period: Timeframe::Week,
            round_agent_idx: 0,
            force_refresh: false,
        };
        d.handle_key(DashboardKey::TabNext);
        assert_eq!(d.tab, Tab::Projects);
        d.handle_key(DashboardKey::TabNext);
        assert_eq!(d.tab, Tab::Rounds);
        d.handle_key(DashboardKey::TabNext);
        assert_eq!(d.tab, Tab::Global);
        d.handle_key(DashboardKey::TabPrev);
        assert_eq!(d.tab, Tab::Rounds);
    }

    #[test]
    fn handle_key_period_arrows() {
        let mut d = TestDash {
            tab: Tab::Global,
            period: Timeframe::Week,
            round_agent_idx: 0,
            force_refresh: false,
        };
        d.handle_key(DashboardKey::Left);
        assert_eq!(d.period, Timeframe::Day);
        d.handle_key(DashboardKey::Left);
        assert_eq!(d.period, Timeframe::Day); // clamp
        d.handle_key(DashboardKey::Right);
        assert_eq!(d.period, Timeframe::Week);
    }

    #[test]
    fn handle_key_refresh_flag() {
        let mut d = TestDash {
            tab: Tab::Global,
            period: Timeframe::Week,
            round_agent_idx: 0,
            force_refresh: false,
        };
        d.handle_key(DashboardKey::Refresh);
        assert!(d.force_refresh);
    }
}

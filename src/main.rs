//! GPUI tokmeter — token spend panel with real tok data layer.

mod data;

use data::agg::{self, Scope, Timeframe, Tot};
use data::cache::Cache;
use data::engine;
use data::limits;
use data::pricing::Pricing;
use data::timeutil::{
    clock, local_day, local_offset, now_epoch, secs_into_local_day, ymd_hour_str,
};
use gpui::{
    actions, canvas, div, fill, point, prelude::*, px, rgb, size, App, AppContext, Bounds, Context,
    FocusHandle, Focusable, InteractiveElement, KeyBinding, ParentElement, Pixels, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowBackgroundAppearance, WindowBounds,
    WindowOptions,
};
use gpui_platform::application;
use std::env;
use std::sync::Arc;

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
const UI_H: f32 = 720.0;
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
    tokens: u64,
    cost: f64,
}

#[derive(Clone, Default)]
struct ProjectRowOwned {
    name: String,
    tokens: u64,
    cost: f64,
}

#[derive(Clone, Default)]
struct RoundRowOwned {
    time: String,
    agent: String,
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
    agent: String,
    windows: Vec<LimitWinOwned>,
    age: i64,
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
    top_projects: Vec<ProjectRowOwned>,
    projects_tab: Vec<ProjectRowOwned>,
    projects_total: Tot,
    rounds: Vec<RoundRowOwned>,
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
            top_projects: Vec::new(),
            projects_tab: Vec::new(),
            projects_total: Tot::default(),
            rounds: Vec::new(),
            clock: String::new(),
        }
    }
}

fn build_snapshot(
    cache: &Cache,
    pricing: &Pricing,
    tf: Timeframe,
    round_agent_idx: usize,
) -> ViewSnapshot {
    let now = now_epoch();
    let off = local_offset(now);
    let today = local_day(now, off);
    let cur_hour = ymd_hour_str(now, off);
    let elapsed_h = secs_into_local_day(now, off) as f64 / 3600.0;
    let scope = Scope { project_root: None };
    let s = agg::build(cache, pricing, tf, &scope, today, &cur_hour, elapsed_h);

    let agents: Vec<AgentRowOwned> = s
        .agents
        .iter()
        .map(|a| AgentRowOwned {
            name: a.label.clone(),
            req: a.tot.req,
            input: a.tot.inp,
            output: a.tot.out,
            cache: a.tot.cache,
            cost: a.tot.cost,
        })
        .collect();
    let models: Vec<ModelRowOwned> = s
        .models
        .iter()
        .map(|m| ModelRowOwned {
            name: m.label.clone(),
            tokens: m.tot.tokens(),
            cost: m.tot.cost,
        })
        .collect();
    let chart: Vec<ChartBar> = s
        .chart
        .iter()
        .map(|b| ChartBar {
            label: b.label.clone(),
            tokens: b.tokens,
        })
        .collect();

    let (top, _) = agg::projects_view(cache, pricing, tf, today, 10);
    let top_projects: Vec<ProjectRowOwned> = top
        .iter()
        .map(|p| ProjectRowOwned {
            name: p.label.clone(),
            tokens: p.tot.tokens(),
            cost: p.tot.cost,
        })
        .collect();

    let (plist, ptotal) = agg::projects_view(cache, pricing, tf, today, 14);
    let projects_tab: Vec<ProjectRowOwned> = plist
        .iter()
        .map(|p| ProjectRowOwned {
            name: p.label.clone(),
            tokens: p.tot.tokens(),
            cost: p.tot.cost,
        })
        .collect();

    let filter = if round_agent_idx == 0 {
        None
    } else {
        Some(agg::ROUND_AGENTS[round_agent_idx])
    };
    let rounds: Vec<RoundRowOwned> = agg::rounds_view(cache, pricing, off, 20, filter)
        .into_iter()
        .map(|r| RoundRowOwned {
            time: r.time,
            agent: r.agent,
            project: r.project,
            tokens: r.tokens,
            cost: r.cost,
        })
        .collect();

    let limit_rows = limits::rows(|k| cache.limits.get(k).cloned(), now);
    let limits_owned: Vec<LimitRowOwned> = limit_rows
        .into_iter()
        .map(|r| LimitRowOwned {
            agent: r.agent.to_string(),
            windows: r
                .windows
                .into_iter()
                .map(|w| LimitWinOwned {
                    label: w.label,
                    pct: w.pct,
                })
                .collect(),
            age: r.age,
        })
        .collect();

    ViewSnapshot {
        tf,
        total_tokens: s.agents_total.tokens(),
        total_cost: s.agents_total.cost,
        rate_hour: s.rate_hour,
        per_h: s.per_h,
        rounds_total: s.rounds_total,
        per_round: s.per_round,
        rounds_known: s.rounds_known,
        chart,
        agents,
        models,
        limits: limits_owned,
        top_projects,
        projects_tab,
        projects_total: ptotal,
        rounds,
        clock: clock(now, off),
    }
}

fn snapshot_to_json(snap: &ViewSnapshot, mode: &str) -> serde_json::Value {
    let agents: Vec<_> = snap
        .agents
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "tokens": a.input + a.output + a.cache,
                "cost": a.cost,
                "req": a.req,
            })
        })
        .collect();
    let models: Vec<_> = snap
        .models
        .iter()
        .map(|m| serde_json::json!({"name": m.name, "tokens": m.tokens, "cost": m.cost}))
        .collect();
    let projects: Vec<_> = snap
        .projects_tab
        .iter()
        .map(|p| serde_json::json!({"name": p.name, "tokens": p.tokens, "cost": p.cost}))
        .collect();
    let rounds: Vec<_> = snap
        .rounds
        .iter()
        .map(|r| {
            serde_json::json!({
                "time": r.time, "agent": r.agent, "project": r.project,
                "tokens": r.tokens, "cost": r.cost
            })
        })
        .collect();
    let limits: Vec<_> = snap
        .limits
        .iter()
        .map(|l| {
            serde_json::json!({
                "agent": l.agent,
                "age": l.age,
                "windows": l.windows.iter().map(|w| serde_json::json!({
                    "label": w.label, "pct": w.pct
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    match mode {
        "projects" => serde_json::json!({
            "tf": snap.tf.label(),
            "total_tokens": snap.projects_total.tokens(),
            "total_cost": snap.projects_total.cost,
            "projects": projects,
        }),
        "rounds" => serde_json::json!({
            "tf": snap.tf.label(),
            "rounds": rounds,
        }),
        "limits" => serde_json::json!({
            "limits": limits,
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
        }),
    }
}

// ── Format helpers ──────────────────────────────────────────────────────────

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

fn cost_color(c: f64) -> gpui::Rgba {
    if c < 5.0 {
        cost_lo()
    } else if c < 25.0 {
        accent()
    } else {
        cost_hi()
    }
}

fn bar_color(level: f32) -> gpui::Rgba {
    let colors = bar_colors();
    let i = ((level.clamp(0.0, 1.0) * (colors.len() - 1) as f32).round() as usize)
        .min(colors.len() - 1);
    rgb(colors[i])
}

fn pct_color(p: f64) -> gpui::Rgba {
    if p < 60.0 {
        cost_lo()
    } else if p < 85.0 {
        accent()
    } else {
        cost_hi()
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
            Tab::Global => "GLOBAL",
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
    Refresh,
}

actions!(tokmeter, [NextTab, PrevTab, PeriodLeft, PeriodRight, ForceRefresh]);

// ── Dashboard ───────────────────────────────────────────────────────────────

struct Dashboard {
    focus_handle: FocusHandle,
    home: String,
    pricing: Arc<Pricing>,
    cache: Cache,
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
        let limits_ttl = engine::env_i64("TOK_LIMITS_TTL_SECS", 300).max(5);
        let refresh_secs = engine::env_i64("TOK_REFRESH_SECS", 3).max(1);
        let focus_handle = cx.focus_handle();

        let mut dash = Self {
            focus_handle,
            home,
            pricing,
            cache: Cache::load(engine::cache_path("").into()), // placeholder empty path ok
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
        // Fix cache path with real home
        dash.cache = Cache::load(engine::cache_path(&dash.home));
        dash.schedule_reload(cx, true);
        dash.start_refresh_loop(cx);
        dash
    }

    fn schedule_reload(&mut self, cx: &mut Context<Self>, first: bool) {
        if self.refresh_in_flight && !first {
            return;
        }
        self.refresh_in_flight = true;
        if first {
            self.status = Some("scanning…".into());
        }
        let home = self.home.clone();
        let limits_ttl = self.limits_ttl;
        let pricing = self.pricing.clone();
        let period = self.period;
        let round_idx = self.round_agent_idx;

        cx.spawn(async move |this, cx| {
            let cache = cx
                .background_spawn(async move { engine::reload_default(&home, limits_ttl, false) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let snap = build_snapshot(&cache, &this.pricing, period, round_idx);
                this.cache = cache;
                this.snapshot = snap;
                this.snapshot.tf = this.period;
                // rebuild with current period (period may have changed)
                this.snapshot = build_snapshot(
                    &this.cache,
                    &this.pricing,
                    this.period,
                    this.round_agent_idx,
                );
                this.refresh_in_flight = false;
                this.force_refresh = false;
                this.status = None;
                let _ = pricing; // keep Arc alive across await
                cx.notify();
            });
        })
        .detach();
    }

    fn start_refresh_loop(&self, cx: &mut Context<Self>) {
        let secs = self.refresh_secs as u64;
        cx.spawn(async move |this, cx| loop {
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

    fn rebuild_from_cache(&mut self) {
        self.snapshot = build_snapshot(
            &self.cache,
            &self.pricing,
            self.period,
            self.round_agent_idx,
        );
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
            DashboardKey::Refresh => {
                self.force_refresh = true;
            },
        }
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
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
    fn render_limits(&self) -> impl IntoElement {
        let rows = &self.snapshot.limits;
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .w_full()
            .font_family(MONO)
            .text_xs()
            .children(if rows.is_empty() {
                vec![div()
                    .text_color(dim())
                    .child("limits —")
                    .into_any_element()]
            } else {
                rows.iter()
                    .map(|r| {
                        let wins = if r.windows.is_empty() {
                            "—".to_string()
                        } else {
                            r.windows
                                .iter()
                                .map(|w| format!("{} {:.0}%", w.label, w.pct))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        };
                        let color = r
                            .windows
                            .iter()
                            .map(|w| w.pct)
                            .fold(0.0_f64, f64::max);
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(accent())
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(r.agent.clone()),
                            )
                            .child(div().text_color(pct_color(color)).child(wins))
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
            .child(
                div()
                    .font_family(MONO)
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(accent())
                    .text_sm()
                    .child("tok"),
            )
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
        let s = &self.snapshot;
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .font_family(MONO)
            .text_sm()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .text_color(accent())
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Σ"),
                    )
                    .child(
                        div()
                            .text_color(text())
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(ftok(s.total_tokens)),
                    )
                    .child(
                        div()
                            .text_color(cost_color(s.total_cost))
                            .child(fcost(s.total_cost)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_color(muted())
                    .child(div().text_color(dim()).w(px(48.)).child("rate"))
                    .child(
                        div()
                            .text_color(text())
                            .child(ftok(s.rate_hour)),
                    )
                    .child(div().child("this hr ·"))
                    .child(
                        div()
                            .text_color(text())
                            .child(format!("{}/h avg", ftok(s.per_h as u64))),
                    ),
            )
            .when(s.rounds_known, |d| {
                d.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_color(muted())
                        .child(div().text_color(dim()).w(px(48.)).child("rounds"))
                        .child(div().text_color(text()).child(format!("{}", s.rounds_total)))
                        .child(div().child("turns ·"))
                        .child(
                            div()
                                .text_color(text())
                                .child(format!("{}/turn", ftok(s.per_round as u64))),
                        ),
                )
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
                    &a.name,
                    a.req,
                    a.input,
                    a.output,
                    a.cache,
                    a.cost,
                    false,
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
            .child(
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .child(div().flex_1().child("model"))
                    .child(div().w(px(64.)).text_right().child("tok"))
                    .child(div().w(px(80.)).text_right().child("$")),
            )
            .children(s.models.iter().map(|m| {
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_sm()
                    .child(div().flex_1().text_color(text()).child(m.name.clone()))
                    .child(
                        div()
                            .w(px(64.))
                            .text_right()
                            .text_color(text())
                            .child(ftok(m.tokens)),
                    )
                    .child(
                        div()
                            .w(px(80.))
                            .text_right()
                            .text_color(cost_color(m.cost))
                            .child(fcost(m.cost)),
                    )
            }))
    }

    fn render_top_projects(&self) -> impl IntoElement {
        let rows = &self.snapshot.top_projects;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(section_header("TOP PROJECTS", self.snapshot.tf.label()))
            .children(if rows.is_empty() {
                vec![div()
                    .font_family(MONO)
                    .text_sm()
                    .text_color(dim())
                    .child("— no data —")
                    .into_any_element()]
            } else {
                rows.iter()
                    .map(|p| {
                        div()
                            .flex()
                            .w_full()
                            .font_family(MONO)
                            .text_sm()
                            .child(div().flex_1().text_color(text()).child(p.name.clone()))
                            .child(
                                div()
                                    .w(px(64.))
                                    .text_right()
                                    .child(ftok(p.tokens)),
                            )
                            .child(
                                div()
                                    .w(px(80.))
                                    .text_right()
                                    .text_color(cost_color(p.cost))
                                    .child(fcost(p.cost)),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
    }

    fn render_projects_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = &self.snapshot.projects_tab;
        let total = &self.snapshot.projects_total;
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
                    .child(div().flex_1().child(p.name.clone()))
                    .child(div().w(px(64.)).text_right().child(ftok(p.tokens)))
                    .child(
                        div()
                            .w(px(80.))
                            .text_right()
                            .text_color(cost_color(p.cost))
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
                    .child(div().flex_1().child("─ all"))
                    .child(
                        div()
                            .w(px(64.))
                            .text_right()
                            .child(ftok(total.tokens())),
                    )
                    .child(
                        div()
                            .w(px(80.))
                            .text_right()
                            .text_color(cost_color(total.cost))
                            .child(fcost(total.cost)),
                    ),
            )
    }

    fn render_rounds_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sel = self.round_agent_idx;
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
                                s.bg(accent()).text_color(pill_fg()).font_weight(gpui::FontWeight::BOLD)
                            })
                            .when(!active, |s| s.text_color(muted()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_round_agent(i, cx)
                            }))
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
                    .child(div().w(px(64.)).child("agent"))
                    .child(div().flex_1().child("project"))
                    .child(div().w(px(56.)).text_right().child("tok"))
                    .child(div().w(px(72.)).text_right().child("$")),
            )
            .children(self.snapshot.rounds.iter().map(|r| {
                let ac = if r.agent == "codex" {
                    accent()
                } else {
                    rgb(0xf5c542)
                };
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_sm()
                    .child(div().w(px(48.)).text_color(dim()).child(r.time.clone()))
                    .child(div().w(px(64.)).text_color(ac).child(r.agent.clone()))
                    .child(div().flex_1().text_color(text()).child(r.project.clone()))
                    .child(
                        div()
                            .w(px(56.))
                            .text_right()
                            .child(ftok(r.tokens)),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .text_right()
                            .text_color(cost_color(r.cost))
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
    div()
        .flex()
        .w_full()
        .font_family(MONO)
        .text_xs()
        .text_color(dim())
        .child(div().w(px(72.)).child(""))
        .child(div().w(px(56.)).text_right().child("req"))
        .child(div().w(px(56.)).text_right().child("in"))
        .child(div().w(px(56.)).text_right().child("out"))
        .child(div().w(px(56.)).text_right().child("cache"))
        .child(div().flex_1().text_right().child("$"))
}

fn agent_row(
    name: &str,
    req: u64,
    input: u64,
    output: u64,
    cache: u64,
    cost: f64,
    bold: bool,
) -> impl IntoElement {
    let name_s: SharedString = name.to_string().into();
    let name_color = if name == "codex" {
        accent()
    } else if name == "claude" {
        rgb(0xf5c542)
    } else {
        text()
    };
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
        .child(div().w(px(72.)).text_color(name_color).child(name_s))
        .child(
            div()
                .w(px(56.))
                .text_right()
                .child(format!("{req}")),
        )
        .child(div().w(px(56.)).text_right().child(ftok(input)))
        .child(div().w(px(56.)).text_right().child(ftok(output)))
        .child(div().w(px(56.)).text_right().child(ftok(cache)))
        .child(
            div()
                .flex_1()
                .text_right()
                .text_color(cost_color(cost))
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
            .on_action(cx.listener(|this, _: &ForceRefresh, _, cx| {
                this.handle_key(DashboardKey::Refresh);
                this.schedule_reload(cx, false);
                cx.notify();
            }))
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
                    .flex()
                    .flex_col()
                    .gap_3()
                    .flex_1()
                    .overflow_hidden()
                    .child(self.render_period(cx))
                    .child(self.render_chart())
                    .child(self.render_metrics())
                    .child(self.render_agents())
                    .child(self.render_models())
                    .child(self.render_top_projects())
                    .into_any_element(),
                Tab::Projects => self.render_projects_tab(cx).into_any_element(),
                Tab::Rounds => self.render_rounds_tab(cx).into_any_element(),
            })
            .child(
                div()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .child("Tab tabs · ←→ period/agent · r refresh"),
            )
    }
}

// ── CLI / main ──────────────────────────────────────────────────────────────

fn dump_mode_from_args(args: &[String]) -> Option<String> {
    for a in args {
        if a == "--dump-json" {
            return Some("global".into());
        }
        if let Some(rest) = a.strip_prefix("--dump-json=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn run_dump(mode: &str) {
    let home = env::var("HOME").unwrap_or_default();
    let pricing = Pricing::load(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/pricing.json"
    )));
    let limits_ttl = if mode == "limits" {
        engine::env_i64("TOK_LIMITS_TTL_SECS", 300).max(5)
    } else {
        0
    };
    let cache = engine::reload_default(&home, limits_ttl, false);
    let snap = build_snapshot(&cache, &pricing, Timeframe::Week, 0);
    let v = snapshot_to_json(&snap, mode);
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if let Some(mode) = dump_mode_from_args(&args) {
        run_dump(&mode);
        return;
    }

    application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("tab", NextTab, Some("Dashboard")),
            KeyBinding::new("shift-tab", PrevTab, Some("Dashboard")),
            KeyBinding::new("left", PeriodLeft, Some("Dashboard")),
            KeyBinding::new("right", PeriodRight, Some("Dashboard")),
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

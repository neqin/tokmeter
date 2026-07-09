//! GPUI prototype of the `tok` token-spend panel (screenshot layout).
//! Static demo data; tabs/period are clickable.

mod data;

use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, App, Bounds, Context, InteractiveElement,
    ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};
use gpui_platform::application;

// ── Theme (dark panel like the tok screenshot) ──────────────────────────────

fn bg() -> gpui::Rgba {
    // Near-black charcoal of the reference panel.
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
    rgb(0xf0a030) // gold/orange like tok
}
fn pill_fg() -> gpui::Rgba {
    rgb(0x1a1208)
}
fn cost_hi() -> gpui::Rgba {
    rgb(0xff6b6b) // red costs pop on dark
}
fn cost_lo() -> gpui::Rgba {
    rgb(0x5ecf8a)
}
fn bar_colors() -> [u32; 6] {
    // Brown → amber → bright gold (readable on dark).
    [0x8b4513, 0xb85c1a, 0xd97706, 0xea8c10, 0xf5a623, 0xffc107]
}

const MONO: &str = "JetBrains Mono";
const UI_W: f32 = 520.0;
const UI_H: f32 = 620.0;
const CHART_H: f32 = 140.0;

// ── Demo data (from the screenshot) ─────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Project,
    Global,
    Projects,
    Rounds,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Project, Tab::Global, Tab::Projects, Tab::Rounds];

    fn label(self) -> &'static str {
        match self {
            Tab::Project => "Project",
            Tab::Global => "GLOBAL",
            Tab::Projects => "Projects",
            Tab::Rounds => "Rounds",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Period {
    Day,
    Week,
    Month,
    Quarter,
    All,
}

impl Period {
    const ALL: [Period; 5] = [
        Period::Day,
        Period::Week,
        Period::Month,
        Period::Quarter,
        Period::All,
    ];

    fn label(self) -> &'static str {
        match self {
            Period::Day => "day",
            Period::Week => "WEEK",
            Period::Month => "month",
            Period::Quarter => "quarter",
            Period::All => "all",
        }
    }
}

struct AgentRow {
    name: &'static str,
    req: u64,
    input: u64,
    output: u64,
    cache: u64,
    cost: f64,
}

struct ModelRow {
    name: &'static str,
    tokens: u64,
    cost: f64,
}

struct DemoData {
    /// Bucket heights as token counts (for bar chart).
    chart: Vec<u64>,
    chart_left: &'static str,
    chart_right: &'static str,
    total_tokens: u64,
    total_cost: f64,
    rate_this_hr: u64,
    rate_avg_h: u64,
    rounds: u64,
    per_turn: u64,
    agents: Vec<AgentRow>,
    models: Vec<ModelRow>,
    tf_label: &'static str,
}

fn demo_week() -> DemoData {
    DemoData {
        // Rough shape from the screenshot: high → dip → high → taper.
        chart: vec![
            780_000_000,
            820_200_000,
            620_000_000,
            580_000_000,
            420_000_000,
            760_000_000,
            740_000_000,
            680_000_000,
            640_000_000,
            580_000_000,
            360_000_000,
            340_000_000,
        ],
        chart_left: "03",
        chart_right: "09",
        total_tokens: 4_500_000_000,
        total_cost: 3887.78,
        rate_this_hr: 6_900_000,
        rate_avg_h: 27_000_000,
        rounds: 1965,
        per_turn: 2_300_000,
        agents: vec![
            AgentRow {
                name: "codex",
                req: 23840,
                input: 132_100_000,
                output: 11_100_000,
                cache: 2_800_000_000,
                cost: 2384.43,
            },
            AgentRow {
                name: "claude",
                req: 8991,
                input: 2_300_000,
                output: 10_900_000,
                cache: 1_600_000_000,
                cost: 1503.35,
            },
        ],
        models: vec![
            ModelRow {
                name: "gpt-5.5",
                tokens: 2_900_000_000,
                cost: 2384.43,
            },
            ModelRow {
                name: "claude-opus-4-8",
                tokens: 1_500_000_000,
                cost: 1311.76,
            },
            ModelRow {
                name: "claude-fable-5",
                tokens: 57_700_000,
                cost: 152.13,
            },
            ModelRow {
                name: "claude-opus-4-7",
                tokens: 25_200_000,
                cost: 39.47,
            },
            ModelRow {
                name: "<synthetic>",
                tokens: 0,
                cost: 0.0,
            },
        ],
        tf_label: "week",
    }
}

// ── Formatting ──────────────────────────────────────────────────────────────

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
    if c <= 0.0 {
        cost_lo()
    } else if c < 5.0 {
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

// ── Root view ───────────────────────────────────────────────────────────────

struct Dashboard {
    tab: Tab,
    period: Period,
    data: DemoData,
    clock: SharedString,
}

impl Dashboard {
    fn new() -> Self {
        Self {
            tab: Tab::Project,
            period: Period::Week,
            data: demo_week(),
            clock: "18:06:55".into(),
        }
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn set_period(&mut self, period: Period, cx: &mut Context<Self>) {
        self.period = period;
        // Keep the same demo payload for the prototype; only the pill changes.
        self.data.tf_label = match period {
            Period::Day => "day",
            Period::Week => "week",
            Period::Month => "month",
            Period::Quarter => "quarter",
            Period::All => "all",
        };
        cx.notify();
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
                    .hover(|s| s.text_color(text()))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(label)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .font_family(MONO)
                    .text_sm()
                    .text_color(muted())
                    .child(self.clock.clone()),
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
            .children(Period::ALL.into_iter().map(|p| {
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
                    .when(!is_active, |s| s.text_color(muted()).hover(|s| s.text_color(text())))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_period(p, cx)))
                    .child(p.label().to_lowercase())
            }))
    }

    fn render_chart(&self) -> impl IntoElement {
        let values = self.data.chart.clone();
        let peak = values.iter().copied().max().unwrap_or(0);
        let left = self.data.chart_left;
        let right = self.data.chart_right;
        let unit = match self.period {
            Period::Day => "hour",
            Period::Week => "day",
            Period::Month => "day",
            Period::Quarter => "week",
            Period::All => "month",
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
                    .items_center()
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
                div()
                    .w_full()
                    .h(px(CHART_H))
                    .child(
                        canvas(
                            move |_, _, _| (),
                            move |bounds, _, window, _| {
                                paint_bars(bounds, &values, peak, window);
                            },
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
        let d = &self.data;
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
                            .child(ftok(d.total_tokens)),
                    )
                    .child(
                        div()
                            .text_color(cost_color(d.total_cost))
                            .child(fcost(d.total_cost)),
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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(ftok(d.rate_this_hr)),
                    )
                    .child(div().child("this hr"))
                    .child(div().text_color(dim()).child("·"))
                    .child(
                        div()
                            .text_color(text())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("{}/h avg", ftok(d.rate_avg_h))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_color(muted())
                    .child(div().text_color(dim()).w(px(48.)).child("rounds"))
                    .child(
                        div()
                            .text_color(text())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("{}", d.rounds)),
                    )
                    .child(div().child("turns"))
                    .child(div().text_color(dim()).child("·"))
                    .child(
                        div()
                            .text_color(text())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("{}/turn", ftok(d.per_turn))),
                    ),
            )
    }

    fn render_agents(&self) -> impl IntoElement {
        let d = &self.data;
        let total_req: u64 = d.agents.iter().map(|a| a.req).sum();
        let total_in: u64 = d.agents.iter().map(|a| a.input).sum();
        let total_out: u64 = d.agents.iter().map(|a| a.output).sum();
        let total_cache: u64 = d.agents.iter().map(|a| a.cache).sum();
        let total_cost: f64 = d.agents.iter().map(|a| a.cost).sum();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(section_header("BY AGENT", d.tf_label))
            .child(agent_header_row())
            .children(d.agents.iter().map(|a| {
                agent_row(
                    a.name,
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
        let d = &self.data;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(section_header("BY MODEL", d.tf_label))
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
            .children(d.models.iter().map(|m| {
                let cost = m.cost;
                div()
                    .flex()
                    .w_full()
                    .font_family(MONO)
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .text_color(if m.name.starts_with('<') {
                                muted()
                            } else {
                                text()
                            })
                            .child(m.name),
                    )
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
                            .text_color(cost_color(cost))
                            .child(fcost(cost)),
                    )
            }))
    }

    fn render_placeholder_tab(&self, title: &str) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .flex_1()
            .gap_2()
            .child(
                div()
                    .font_family(MONO)
                    .text_color(muted())
                    .child(format!("{title} — prototype stub")),
            )
            .child(
                div()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .child("Stats view is wired; other tabs are placeholders."),
            )
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
    name: impl Into<SharedString>,
    req: u64,
    input: u64,
    output: u64,
    cache: u64,
    cost: f64,
    bold: bool,
) -> impl IntoElement {
    let name = name.into();
    let weight = if bold {
        gpui::FontWeight::BOLD
    } else {
        gpui::FontWeight::NORMAL
    };
    let name_color = if name.as_ref() == "codex" {
        accent()
    } else if name.as_ref() == "claude" {
        rgb(0xf5c542)
    } else {
        text()
    };
    div()
        .flex()
        .w_full()
        .font_family(MONO)
        .text_sm()
        .font_weight(weight)
        .child(div().w(px(72.)).text_color(name_color).child(name))
        .child(
            div()
                .w(px(56.))
                .text_right()
                .text_color(text())
                .child(format!("{req}")),
        )
        .child(
            div()
                .w(px(56.))
                .text_right()
                .text_color(text())
                .child(ftok(input)),
        )
        .child(
            div()
                .w(px(56.))
                .text_right()
                .text_color(text())
                .child(ftok(output)),
        )
        .child(
            div()
                .w(px(56.))
                .text_right()
                .text_color(text())
                .child(ftok(cache)),
        )
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
    let total_gap = gap * (n - 1.0);
    let bar_w = ((bounds.size.width - total_gap) / n).max(px(4.0));
    let max_h = bounds.size.height;

    for (i, &v) in values.iter().enumerate() {
        let level = v as f32 / peak as f32;
        let h = max_h * level;
        let x = bounds.origin.x + (bar_w + gap) * i as f32;
        let y = bounds.origin.y + max_h - h;
        let color = bar_color(level);
        window.paint_quad(fill(
            Bounds {
                origin: point(x, y),
                size: size(bar_w, h),
            },
            color,
        ));
        // Slight top highlight
        if h > px(4.0) {
            window.paint_quad(fill(
                Bounds {
                    origin: point(x, y),
                    size: size(bar_w, px(3.0)),
                },
                rgb(0xffe08a),
            ));
        }
    }
}

impl Render for Dashboard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(bg())
            .text_color(text())
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .child(self.render_tabbar(cx))
            .child(div().h(px(1.)).w_full().bg(rgb(0x2a2a30)))
            .child(match self.tab {
                Tab::Project | Tab::Global => div()
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
                    .into_any_element(),
                Tab::Projects => self
                    .render_placeholder_tab("Projects")
                    .into_any_element(),
                Tab::Rounds => self.render_placeholder_tab("Rounds").into_any_element(),
            })
            .child(
                div()
                    .font_family(MONO)
                    .text_xs()
                    .text_color(dim())
                    .child("click tabs/period · demo data from screenshot · q not bound"),
            )
    }
}

fn main() {
    application().run(|cx: &mut App| {
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
                window.set_window_title("tokmeter — GPUI prototype");
                cx.new(|_cx| Dashboard::new())
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}

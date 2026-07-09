//! Лимиты подписок агентов. Claude: OAuth-эндпоинт usage (по токену из
//! ~/.claude/.credentials.json, curl без токена в argv). Codex: ChatGPT
//! backend-api/codex/usage плюс пассивный снапшот rate_limits из событий
//! token_count сессии; с API-ключом окон нет (там pay-as-you-go).

use super::timeutil::parse_epoch;
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

/// Одно окно лимита: подпись ("5h"/"wk"/имя модели), процент, epoch сброса
/// (0 — неизвестен).
#[derive(Clone)]
pub struct Window {
    pub label: String,
    pub pct: f64,
    pub resets: i64,
}

/// Снапшот лимитов агента. ts — когда данные получены; checked — когда
/// последний раз пытались обновить (бэкофф при ошибках сети).
#[derive(Clone, Default)]
pub struct Snapshot {
    pub ts: i64,
    pub checked: i64,
    pub windows: Vec<Window>,
}

/// Строка для рендера: окна с уже отфильтрованными сброшенными, age в секундах.
pub struct Row {
    pub agent: &'static str,
    pub windows: Vec<Window>,
    pub age: i64,
}

/// Подготовить строки claude/codex к рендеру: выкинуть окна, чей resets в
/// прошлом (данные пережили сброс — процент уже не тот).
pub fn rows(get: impl Fn(&str) -> Option<Snapshot>, now: i64) -> Vec<Row> {
    ["claude", "codex"]
        .iter()
        .map(|agent| {
            let snap = get(agent).unwrap_or_default();
            let windows = snap
                .windows
                .into_iter()
                .filter(|w| w.resets == 0 || w.resets >= now)
                .filter(|w| *agent != "codex" || matches!(w.label.as_str(), "5h" | "wk"))
                .collect();
            Row {
                agent,
                windows,
                age: if snap.ts == 0 { 0 } else { now - snap.ts },
            }
        })
        .collect()
}

/// Запросить лимиты Claude по OAuth-токену. Токен передаём через stdin-конфиг
/// curl (-K -), чтобы не светить его в argv.
pub fn fetch_claude(home: &str, now: i64) -> Option<Snapshot> {
    let text = std::fs::read_to_string(format!("{home}/.claude/.credentials.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let token = oauth.get("accessToken").and_then(|x| x.as_str())?;
    let expires_ms = oauth.get("expiresAt").and_then(|x| x.as_i64()).unwrap_or(0);
    if expires_ms / 1000 <= now {
        return None; // токен протух — обновит сам Claude Code при следующем запуске
    }

    let mut child = Command::new("curl")
        .args(["-sf", "-m", "3", "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let cfg = format!(
        "url = \"https://api.anthropic.com/api/oauth/usage\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"anthropic-beta: oauth-2025-04-20\"\n"
    );
    child.stdin.take()?.write_all(cfg.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_claude(&String::from_utf8_lossy(&out.stdout), now)
}

/// Запросить лимиты Codex через ChatGPT auth. Если access token протух,
/// оставляем старый общий кэш; refresh сделает сам Codex при следующем запуске.
pub fn fetch_codex(home: &str, now: i64) -> Option<Snapshot> {
    let text = std::fs::read_to_string(format!("{home}/.codex/auth.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v.get("auth_mode").and_then(|x| x.as_str()) != Some("chatgpt") {
        return None;
    }
    let tokens = v.get("tokens")?;
    let token = tokens.get("access_token").and_then(|x| x.as_str())?;
    let account_id = tokens.get("account_id").and_then(|x| x.as_str())?;
    let mut child = Command::new("curl")
        .args(["-sf", "-m", "5", "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let cfg = format!(
        "url = \"https://chatgpt.com/backend-api/wham/usage\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"ChatGPT-Account-Id: {account_id}\"\n\
         header = \"Accept: application/json\"\n\
         header = \"User-Agent: tok/limits codex-cli\"\n"
    );
    child.stdin.take()?.write_all(cfg.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_codex_usage(&String::from_utf8_lossy(&out.stdout), now)
}

/// Разобрать ответ usage-эндпоинта: массив limits (session / weekly_all /
/// weekly_scoped), фолбэк — поля five_hour/seven_day.
fn parse_claude(body: &str, now: i64) -> Option<Snapshot> {
    let v: Value = serde_json::from_str(body).ok()?;
    let mut windows = Vec::new();
    if let Some(limits) = v.get("limits").and_then(|x| x.as_array()) {
        for l in limits {
            let pct = match l.get("percent").and_then(|x| x.as_f64()) {
                Some(p) => p,
                None => continue,
            };
            let label = match l.get("kind").and_then(|x| x.as_str()) {
                Some("session") => "5h".to_string(),
                Some("weekly_all") => "wk".to_string(),
                Some("weekly_scoped") => l
                    .pointer("/scope/model/display_name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("model")
                    .to_string(),
                _ => continue,
            };
            windows.push(Window {
                label,
                pct,
                resets: resets_epoch(l.get("resets_at")),
            });
        }
    }
    if windows.is_empty() {
        for (key, label) in [("five_hour", "5h"), ("seven_day", "wk")] {
            if let Some(w) = v.get(key) {
                if let Some(pct) = w.get("utilization").and_then(|x| x.as_f64()) {
                    windows.push(Window {
                        label: label.to_string(),
                        pct,
                        resets: resets_epoch(w.get("resets_at")),
                    });
                }
            }
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(Snapshot {
        ts: now,
        checked: now,
        windows,
    })
}

fn parse_codex_usage(body: &str, now: i64) -> Option<Snapshot> {
    let v: Value = serde_json::from_str(body).ok()?;
    let mut windows = Vec::new();
    if let Some(rl) = v.get("rate_limit") {
        push_usage_windows(&mut windows, rl, now);
    }
    if windows.is_empty() {
        return None;
    }
    Some(Snapshot {
        ts: now,
        checked: now,
        windows,
    })
}

fn push_usage_windows(windows: &mut Vec<Window>, rl: &Value, now: i64) {
    for (key, fallback) in [("primary_window", "5h"), ("secondary_window", "wk")] {
        let Some(w) = rl.get(key) else { continue };
        let Some(pct) = w.get("used_percent").and_then(|x| x.as_f64()) else {
            continue;
        };
        let seconds = w
            .get("limit_window_seconds")
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        let label = window_label(seconds / 60, fallback).to_string();
        let resets = w
            .get("reset_at")
            .and_then(|x| x.as_i64())
            .or_else(|| {
                w.get("reset_after_seconds")
                    .and_then(|x| x.as_i64())
                    .map(|s| now + s)
            })
            .unwrap_or(0);
        windows.push(Window { label, pct, resets });
    }
}

fn window_label(mins: i64, fallback: &str) -> &str {
    if mins > 0 && mins <= 360 {
        "5h"
    } else if mins >= 9000 {
        "wk"
    } else {
        fallback
    }
}

/// Снапшот из payload token_count Codex-сессии (rate_limits.primary/secondary).
pub fn codex_snapshot(p: &Value, ts: i64) -> Option<Snapshot> {
    let rl = p.get("rate_limits")?;
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let w = match rl.get(key) {
            Some(w) if w.is_object() => w,
            _ => continue,
        };
        let pct = match w.get("used_percent").and_then(|x| x.as_f64()) {
            Some(p) => p,
            None => continue,
        };
        let mins = w
            .get("window_minutes")
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        let label = if mins > 0 && mins <= 360 {
            "5h".to_string()
        } else if mins >= 9000 {
            "wk".to_string()
        } else if mins > 0 {
            format!("{}h", mins / 60)
        } else if key == "primary" {
            "5h".to_string()
        } else {
            "wk".to_string()
        };
        let resets = match w.get("resets_in_seconds").and_then(|x| x.as_i64()) {
            Some(s) => ts + s,
            None => resets_epoch(w.get("resets_at")),
        };
        windows.push(Window { label, pct, resets });
    }
    if windows.is_empty() {
        return None;
    }
    Some(Snapshot {
        ts,
        checked: ts,
        windows,
    })
}

/// resets_at бывает ISO-строкой или epoch-числом (сек/мс).
fn resets_epoch(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::String(s)) => parse_epoch(s).unwrap_or(0),
        Some(Value::Number(n)) => {
            let x = n.as_i64().unwrap_or(0);
            if x > 1_000_000_000_000 {
                x / 1000
            } else {
                x
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_direct_codex_usage() {
        let body = r#"{
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 6,
                    "limit_window_seconds": 18000,
                    "reset_at": 1783180807
                },
                "secondary_window": {
                    "used_percent": 80,
                    "limit_window_seconds": 604800,
                    "reset_at": 1783576850
                }
            },
            "additional_rate_limits": [{
                "limit_name": "GPT-5.3-Codex-Spark",
                "metered_feature": "codex_bengalfox",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 0,
                        "limit_window_seconds": 18000,
                        "reset_at": 1783182268
                    },
                    "secondary_window": {
                        "used_percent": 0,
                        "limit_window_seconds": 604800,
                        "reset_at": 1783769068
                    }
                }
            }]
        }"#;
        let snap = parse_codex_usage(body, 1783160000).unwrap();
        let got: Vec<_> = snap
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.pct as i64, w.resets))
            .collect();
        assert_eq!(got, vec![("5h", 6, 1783180807), ("wk", 80, 1783576850)]);
    }

    #[test]
    fn rows_hide_additional_codex_windows() {
        let snap = Snapshot {
            ts: 1783160000,
            checked: 1783160000,
            windows: vec![
                Window {
                    label: "5h".to_string(),
                    pct: 6.0,
                    resets: 1783180807,
                },
                Window {
                    label: "wk".to_string(),
                    pct: 80.0,
                    resets: 1783576850,
                },
                Window {
                    label: "Spark 5h".to_string(),
                    pct: 0.0,
                    resets: 1783182268,
                },
            ],
        };
        let got = rows(
            |agent| {
                if agent == "codex" {
                    Some(snap.clone())
                } else {
                    None
                }
            },
            1783160000,
        );
        let codex = got.iter().find(|r| r.agent == "codex").unwrap();
        let labels: Vec<_> = codex.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec!["5h", "wk"]);
    }
}

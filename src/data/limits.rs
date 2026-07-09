//! Лимиты подписок агентов.
//! - Claude: OAuth usage (`~/.claude/.credentials.json`)
//! - Codex: ChatGPT wham/usage + passive token_count rate_limits
//! - Grok: `grok agent stdio` → `_x.ai/billing` (weekly SuperGrok bar);
//!   fallback TPM/RPM only when no weekly snap is available

use super::timeutil::parse_epoch;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

/// Подготовить строки агентов к рендеру: выкинуть окна, чей resets в
/// прошлом (данные пережили сброс — процент уже не тот).
pub fn rows(get: impl Fn(&str) -> Option<Snapshot>, now: i64) -> Vec<Row> {
    ["claude", "codex", "grok"]
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

/// Grok SuperGrok weekly usage via non-public ACP RPC (same spirit as
/// Claude/Codex OAuth usage endpoints).
///
/// Primary: `grok agent stdio` → `authenticate(cached_token)` → `_x.ai/billing`
/// returns `config.creditUsagePercent` + weekly period end (Settings→Usage).
/// Fallback: api.x.ai rate-limit headers (`api` bar) — only when there is no
/// existing weekly/monthly subscription snap to preserve.
pub fn fetch_grok(home: &str, now: i64, prev: Option<&Snapshot>) -> Option<Snapshot> {
    if let Some(snap) = fetch_grok_cli_billing(home, now) {
        return Some(snap);
    }
    // TPM/RPM headers are not the SuperGrok weekly quota. Never clobber a
    // real `wk`/`mo` bar with `api 0%`.
    if grok_has_subscription_bar(prev) {
        return None;
    }
    fetch_grok_api_headers(home, now)
}

/// True when a prior snap already carries SuperGrok weekly/monthly usage.
fn grok_has_subscription_bar(prev: Option<&Snapshot>) -> bool {
    prev.is_some_and(|s| {
        s.windows
            .iter()
            .any(|w| matches!(w.label.as_str(), "wk" | "mo"))
    })
}

fn resolve_grok_bin(home: &str) -> PathBuf {
    let local = Path::new(home).join(".grok/bin/grok");
    if local.is_file() {
        return local;
    }
    PathBuf::from("grok")
}

/// Kill + wait on drop so early `?` after spawn cannot leave a zombie.
struct ReapOnDrop(std::process::Child);

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `grok agent stdio`, auth with cached OAuth, call `_x.ai/billing`.
/// Reads NDJSON until `creditUsagePercent` appears or a hard deadline hits.
fn fetch_grok_cli_billing(home: &str, now: i64) -> Option<Snapshot> {
    let mut child = ReapOnDrop(
        Command::new(resolve_grok_bin(home))
            .args(["agent", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?,
    );
    let mut stdin = child.0.stdin.take()?;
    let stdout = child.0.stdout.take()?;

    let send = |stdin: &mut std::process::ChildStdin, line: &str| -> Option<()> {
        stdin.write_all(line.as_bytes()).ok()?;
        stdin.write_all(b"\n").ok()?;
        stdin.flush().ok()?;
        Some(())
    };

    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1","clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}}"#,
    )?;
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"methodId":"cached_token"}}"#,
    )?;
    // Non-public extension (underscore-prefixed). Public names like
    // session/billing and x.ai/billing return Method not found on agent stdio.
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":3,"method":"_x.ai/billing","params":{}}"#,
    )?;
    // Keep stdin open until reap: closing early makes grok exit before billing.

    // Agent emits skills-reload / announcements between RPC replies — scan the
    // whole buffer for creditUsagePercent instead of waiting for consecutive ids.
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut all = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    all.push_str(&line);
                    if parse_grok_session_billing(&all, now).is_some() {
                        let _ = tx.send(all);
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(all);
    });

    let mut buf = rx.recv_timeout(Duration::from_secs(8)).unwrap_or_default();
    // Close stdin + kill so the reader hits EOF and can flush remaining lines.
    drop(stdin);
    drop(child);
    if let Ok(more) = rx.try_recv() {
        if more.len() > buf.len() {
            buf = more;
        }
    } else if buf.is_empty() {
        if let Ok(more) = rx.recv_timeout(Duration::from_millis(300)) {
            buf = more;
        }
    }
    if buf.is_empty() {
        return None;
    }
    parse_grok_session_billing(&buf, now)
}

fn parse_grok_session_billing(stdout: &str, now: i64) -> Option<Snapshot> {
    // Scan all NDJSON responses for id=3 result with creditUsagePercent.
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // Prefer explicit billing result; also accept any object with creditUsagePercent.
        let cfg = v
            .pointer("/result/config")
            .or_else(|| v.get("config"))
            .cloned();
        let Some(cfg) = cfg else { continue };
        let pct = cfg
            .get("creditUsagePercent")
            .and_then(|x| x.as_f64())
            .or_else(|| {
                // alternate shapes
                cfg.pointer("/usage/creditUsagePercent")
                    .and_then(|x| x.as_f64())
            })?;
        let end = cfg
            .pointer("/currentPeriod/end")
            .or_else(|| cfg.get("billingPeriodEnd"))
            .and_then(|x| x.as_str());
        let resets = end.and_then(parse_epoch).unwrap_or(0);
        let label = match cfg
            .pointer("/currentPeriod/type")
            .and_then(|x| x.as_str())
            .unwrap_or("")
        {
            t if t.contains("WEEKLY") || t.contains("weekly") => "wk",
            t if t.contains("MONTHLY") || t.contains("monthly") => "mo",
            _ if resets > 0 => "wk",
            _ => "wk",
        };
        return Some(Snapshot {
            ts: now,
            checked: now,
            windows: vec![Window {
                label: label.to_string(),
                pct: pct.clamp(0.0, 100.0),
                resets,
            }],
        });
    }
    None
}

/// Fallback: TPM/RPM used % from api.x.ai rate-limit headers.
fn fetch_grok_api_headers(home: &str, now: i64) -> Option<Snapshot> {
    let text = std::fs::read_to_string(format!("{home}/.grok/auth.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let token = v
        .as_object()?
        .values()
        .find_map(|entry| entry.get("key").and_then(|x| x.as_str()))?;
    if token.is_empty() {
        return None;
    }

    let pid = std::process::id();
    let hdr_path = format!("/tmp/tokmeter-grok-hdr-{pid}");
    let body_path = format!("/tmp/tokmeter-grok-body-{pid}");
    let body = r#"{"model":"grok-3","messages":[{"role":"user","content":"."}],"max_tokens":1}"#;
    let _ = std::fs::write(&body_path, body);

    let mut child = Command::new("curl")
        .args([
            "-s",
            "-m",
            "8",
            "-K",
            "-",
            "-D",
            &hdr_path,
            "-o",
            "/dev/null",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let cfg = format!(
        "url = \"https://api.x.ai/v1/chat/completions\"\n\
         header = \"Authorization: Bearer {token}\"\n\
         header = \"Content-Type: application/json\"\n\
         header = \"Accept: application/json\"\n\
         data = \"@{body_path}\"\n"
    );
    child.stdin.take()?.write_all(cfg.as_bytes()).ok()?;
    let _ = child.wait();
    let hdr = std::fs::read_to_string(&hdr_path).unwrap_or_default();
    let _ = std::fs::remove_file(&hdr_path);
    let _ = std::fs::remove_file(&body_path);
    if hdr.is_empty() {
        return None;
    }
    parse_grok_headers(&hdr, now)
}

fn parse_grok_headers(headers: &str, now: i64) -> Option<Snapshot> {
    let mut limit_tok: Option<f64> = None;
    let mut rem_tok: Option<f64> = None;
    let mut limit_req: Option<f64> = None;
    let mut rem_req: Option<f64> = None;
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        let Some((k, v)) = lower.split_once(':') else {
            continue;
        };
        let v = v.trim();
        let n = v.parse::<f64>().ok();
        match k.trim() {
            "x-ratelimit-limit-tokens" => limit_tok = n,
            "x-ratelimit-remaining-tokens" => rem_tok = n,
            "x-ratelimit-limit-requests" => limit_req = n,
            "x-ratelimit-remaining-requests" => rem_req = n,
            _ => {}
        }
    }
    let (label, limit, rem) = match (limit_tok, rem_tok) {
        (Some(l), Some(r)) if l > 0.0 => ("api", l, r),
        _ => match (limit_req, rem_req) {
            (Some(l), Some(r)) if l > 0.0 => ("rpm", l, r),
            _ => return None,
        },
    };
    let used = ((limit - rem) / limit * 100.0).clamp(0.0, 100.0);
    Some(Snapshot {
        ts: now,
        checked: now,
        windows: vec![Window {
            label: label.to_string(),
            pct: used,
            resets: 0,
        }],
    })
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
        assert!(got.iter().any(|r| r.agent == "grok"));
    }

    #[test]
    fn parses_grok_rate_limit_headers() {
        let hdr = "\
HTTP/2 200\r\n\
x-ratelimit-limit-tokens: 15000000\r\n\
x-ratelimit-remaining-tokens: 7500000\r\n\
x-ratelimit-limit-requests: 900\r\n\
x-ratelimit-remaining-requests: 900\r\n\
";
        let snap = parse_grok_headers(hdr, 100).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "api");
        assert!((snap.windows[0].pct - 50.0).abs() < 0.01);
    }

    #[test]
    fn parses_grok_session_billing_weekly() {
        let stdout = r#"
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":2,"result":{"_meta":{"subscription_tier":"SuperGrok Heavy"}}}
{"jsonrpc":"2.0","id":3,"result":{"config":{"creditUsagePercent":2.0,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-08T18:43:04.007622+00:00","end":"2026-07-15T18:43:04.007622+00:00"},"billingPeriodEnd":"2026-07-15T18:43:04.007622+00:00"}}}
"#;
        let snap = parse_grok_session_billing(stdout, 1_000).unwrap();
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "wk");
        assert!((snap.windows[0].pct - 2.0).abs() < 0.01);
        assert!(snap.windows[0].resets > 0);
    }

    #[test]
    fn grok_preserves_subscription_bar() {
        let wk = Snapshot {
            ts: 1,
            checked: 1,
            windows: vec![Window {
                label: "wk".to_string(),
                pct: 4.0,
                resets: 99,
            }],
        };
        let mo = Snapshot {
            ts: 1,
            checked: 1,
            windows: vec![Window {
                label: "mo".to_string(),
                pct: 10.0,
                resets: 99,
            }],
        };
        let api = Snapshot {
            ts: 1,
            checked: 1,
            windows: vec![Window {
                label: "api".to_string(),
                pct: 0.0,
                resets: 0,
            }],
        };
        let empty = Snapshot::default();
        assert!(grok_has_subscription_bar(Some(&wk)));
        assert!(grok_has_subscription_bar(Some(&mo)));
        assert!(!grok_has_subscription_bar(Some(&api)));
        assert!(!grok_has_subscription_bar(Some(&empty)));
        assert!(!grok_has_subscription_bar(None));
    }
}

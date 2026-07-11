//! Инкрементальный разбор сессий Claude/Codex/OMP. Открываем файл только если он
//! вырос; читаем лишь дописанный хвост; учитываем каждый API-запрос один раз.

use super::cache::{Cache, FileState};
use super::limits;
use super::timeutil::{local_day, parse_epoch, ymd_hour_str, ymd_str};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Source {
    Claude,
    Codex,
    Omp,
}

pub struct Scanner {
    claude_root: PathBuf,
    codex_root: PathBuf,
    omp_root: PathBuf,
    min_mtime: i64,
    off: i64,      // локальное смещение для датирования
    ring_min: i64, // нижняя граница ts для кольца раундов (epoch)
}

impl Scanner {
    pub fn new(home: &str, min_mtime: i64, off: i64, ring_min: i64) -> Scanner {
        Scanner {
            claude_root: Path::new(home).join(".claude").join("projects"),
            codex_root: Path::new(home).join(".codex").join("sessions"),
            // oh-my-pi / omp: полноценный per-turn usage в jsonl
            omp_root: Path::new(home).join(".omp").join("agent").join("sessions"),
            min_mtime,
            off,
            ring_min,
        }
    }

    /// Подхватить новые/выросшие файлы и дописать агрегаты в кэш.
    pub fn update(&self, cache: &mut Cache) {
        let mut files: Vec<(PathBuf, u64, i64, Source)> = Vec::new();
        walk(&self.claude_root, self.min_mtime, Source::Claude, &mut files);
        walk(&self.codex_root, self.min_mtime, Source::Codex, &mut files);
        walk(&self.omp_root, self.min_mtime, Source::Omp, &mut files);

        for (path, size, mtime, source) in &files {
            let key = path.to_string_lossy().into_owned();
            let st = cache.files.get(&key).cloned().unwrap_or_default();
            if st.size == *size && st.mtime == *mtime {
                continue; // не изменился — даже не открываем
            }
            let mut st = st;
            if let Some(chunk) = read_tail(path, *size, &mut st) {
                for line in chunk.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    let v: Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    match source {
                        Source::Claude => self.claude_line(&v, path, &mut st, cache),
                        Source::Codex => self.codex_line(&v, &mut st, cache),
                        Source::Omp => self.omp_line(&v, path, &mut st, cache),
                    }
                }
            }
            st.size = *size;
            st.mtime = *mtime;
            cache.files.insert(key, st);
            cache.dirty = true;
        }
    }

    fn date_of(&self, ts: &str) -> Option<String> {
        parse_epoch(ts).map(|e| ymd_str(local_day(e, self.off)))
    }

    fn hour_of(&self, ts: &str) -> Option<String> {
        parse_epoch(ts).map(|e| ymd_hour_str(e, self.off))
    }

    fn claude_line(&self, o: &Value, path: &Path, st: &mut FileState, cache: &mut Cache) {
        match o.get("type").and_then(|x| x.as_str()) {
            Some("user") => {
                self.claude_round(o, path, st, cache);
                return;
            }
            Some("assistant") => {}
            _ => return,
        }
        let msg = &o["message"];
        let usage = &msg["usage"];
        let mid = match msg.get("id").and_then(|x| x.as_str()) {
            Some(s) => s,
            None => return,
        };
        if usage.is_null() {
            return;
        }
        if mid == st.msgid {
            return; // повтор строки того же ответа
        }
        st.msgid = mid.to_string();

        let u = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let inp = u("input_tokens");
        let cread = u("cache_read_input_tokens");
        let cc = &usage["cache_creation"];
        let (cw5, cw1h) = if cc.is_object() {
            (
                cc.get("ephemeral_5m_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                cc.get("ephemeral_1h_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            )
        } else {
            (u("cache_creation_input_tokens"), 0)
        };
        let out = u("output_tokens");

        let mut speed = usage
            .get("speed")
            .or_else(|| usage.get("service_tier"))
            .and_then(|x| x.as_str())
            .unwrap_or("standard");
        if speed != "fast" {
            speed = "standard";
        }
        let model = msg
            .get("model")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        let date = match o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|t| self.date_of(t))
        {
            Some(d) => d,
            None => return,
        };
        let project = o
            .get("cwd")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| decode_folder(path));

        cache.add(
            &date, "claude", model, speed, &project, inp, cread, cw5, cw1h, out,
        );
        if let Some(hour) = o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|t| self.hour_of(t))
        {
            cache.add_hour_tokens(&hour, "claude", &project, inp + cread + cw5 + cw1h + out);
        }
        if st.r_ts != 0 {
            st.r_in += inp;
            st.r_cread += cread;
            st.r_cw5 += cw5;
            st.r_cw1h += cw1h;
            st.r_out += out;
            st.r_model = model.to_string();
            st.r_speed = speed.to_string();
        }
    }

    /// Пользовательский ход Claude — считаем раунд по новому promptId.
    fn claude_round(&self, o: &Value, path: &Path, st: &mut FileState, cache: &mut Cache) {
        let msg = &o["message"];
        if msg.get("role").and_then(|x| x.as_str()) != Some("user") {
            return;
        }
        let pid = match o.get("promptId").and_then(|x| x.as_str()) {
            Some(s) => s,
            None => return,
        };
        if pid == st.last_pid {
            return;
        }
        let content = &msg["content"];
        let text = if let Some(s) = content.as_str() {
            s.to_string()
        } else if let Some(arr) = content.as_array() {
            arr.iter()
                .find_map(|b| {
                    if b.get("type").and_then(|x| x.as_str()) == Some("text") {
                        b.get("text").and_then(|x| x.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        let t = text.trim();
        if t.is_empty() || t.starts_with('<') {
            return; // результат инструмента / системное — не раунд
        }
        st.last_pid = pid.to_string();
        cache.flush_round(st, "claude", self.ring_min);
        let hour = match o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|s| self.hour_of(s))
        {
            Some(h) => h,
            None => return,
        };
        let project = o
            .get("cwd")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| decode_folder(path));
        st.r_ts = o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(parse_epoch)
            .unwrap_or(0);
        st.r_proj = project.clone();
        cache.add_round(&hour, "claude", &project);
    }

    fn codex_line(&self, o: &Value, st: &mut FileState, cache: &mut Cache) {
        let t = o.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let p = &o["payload"];
        if st.codex_replay {
            if t == "inter_agent_communication_metadata" {
                st.codex_replay = false;
                st.codex_parent.clear();
                st.ptotal = 0;
            }
            return;
        }
        match t {
            "session_meta" => {
                let id = p.get("id").and_then(|x| x.as_str()).unwrap_or("");
                if !st.codex_parent.is_empty() && id == st.codex_parent {
                    st.codex_replay = true;
                    return;
                }
                if st.codex_parent.is_empty() {
                    st.codex_parent = p
                        .pointer("/source/subagent/thread_spawn/parent_thread_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                if let Some(c) = p.get("cwd").and_then(|x| x.as_str()) {
                    st.proj = c.to_string();
                }
                if let Some(m) = p.get("model").and_then(|x| x.as_str()) {
                    st.model = m.to_string();
                }
            }
            "turn_context" => {
                if let Some(c) = p.get("cwd").and_then(|x| x.as_str()) {
                    st.proj = c.to_string();
                }
                if let Some(m) = p.get("model").and_then(|x| x.as_str()) {
                    st.model = m.to_string();
                }
            }
            "event_msg" => match p.get("type").and_then(|x| x.as_str()) {
                Some("user_message") => {
                    let text = p.get("message").and_then(|x| x.as_str()).unwrap_or("");
                    let t = text.trim();
                    if t.is_empty() || t.starts_with('<') {
                        return;
                    }
                    cache.flush_round(st, "codex", self.ring_min);
                    let project = if st.proj.is_empty() {
                        "unknown".to_string()
                    } else {
                        st.proj.clone()
                    };
                    if let Some(hour) = o
                        .get("timestamp")
                        .and_then(|x| x.as_str())
                        .and_then(|s| self.hour_of(s))
                    {
                        cache.add_round(&hour, "codex", &project);
                    }
                    st.r_ts = o
                        .get("timestamp")
                        .and_then(|x| x.as_str())
                        .and_then(parse_epoch)
                        .unwrap_or(0);
                    st.r_proj = project;
                }
                Some("token_count") => {
                    // rate_limits (ChatGPT-план) — до дедупа по total_tokens:
                    // финальные события повторяют тоталы, но несут свежие лимиты
                    if let Some(ts) = o
                        .get("timestamp")
                        .and_then(|x| x.as_str())
                        .and_then(parse_epoch)
                    {
                        if let Some(snap) = limits::codex_snapshot(p, ts) {
                            cache.set_limits("codex", snap);
                        }
                    }
                    let info = &p["info"];
                    let tot = match info["total_token_usage"]
                        .get("total_tokens")
                        .and_then(|x| x.as_u64())
                    {
                        Some(v) => v,
                        None => return,
                    };
                    if tot == st.ptotal {
                        return; // дубликат / финальная мусорная строка
                    }
                    st.ptotal = tot;
                    let last = &info["last_token_usage"];
                    let l = |k: &str| last.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                    let lin = l("input_tokens");
                    let cached = l("cached_input_tokens");
                    let out = l("output_tokens");
                    let inp = lin.saturating_sub(cached);
                    let date = match o
                        .get("timestamp")
                        .and_then(|x| x.as_str())
                        .and_then(|t| self.date_of(t))
                    {
                        Some(d) => d,
                        None => return,
                    };
                    let hour = o
                        .get("timestamp")
                        .and_then(|x| x.as_str())
                        .and_then(|t| self.hour_of(t));
                    let model = if st.model.is_empty() {
                        "codex"
                    } else {
                        &st.model
                    };
                    let project = if st.proj.is_empty() {
                        "unknown"
                    } else {
                        &st.proj
                    };
                    cache.add(
                        &date, "codex", model, "standard", project, inp, cached, 0, 0, out,
                    );
                    if let Some(hour) = hour {
                        cache.add_hour_tokens(&hour, "codex", project, inp + cached + out);
                    }
                    if st.r_ts != 0 {
                        st.r_in += inp;
                        st.r_cread += cached;
                        st.r_out += out;
                        st.r_model = if st.model.is_empty() {
                            "codex".to_string()
                        } else {
                            st.model.clone()
                        };
                        st.r_speed = "standard".to_string();
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// OMP / oh-my-pi: `~/.omp/agent/sessions/**/*.jsonl`.
    /// `session` → cwd; `message.role=user` → раунд; `assistant`+`usage` → токены.
    fn omp_line(&self, o: &Value, path: &Path, st: &mut FileState, cache: &mut Cache) {
        match o.get("type").and_then(|x| x.as_str()) {
            Some("session") | Some("session_init") => {
                if let Some(c) = o.get("cwd").and_then(|x| x.as_str()) {
                    st.proj = c.to_string();
                }
                if let Some(m) = o.get("model").and_then(|x| x.as_str()) {
                    st.model = m.to_string();
                }
            }
            Some("message") => {
                let msg = &o["message"];
                match msg.get("role").and_then(|x| x.as_str()) {
                    Some("user") => self.omp_round(o, path, st, cache),
                    Some("assistant") => self.omp_usage(o, path, st, cache),
                    _ => {}
                }
            }
            Some("model_change") => {
                if let Some(m) = o
                    .get("model")
                    .or_else(|| o.get("to"))
                    .and_then(|x| x.as_str())
                {
                    st.model = m.to_string();
                }
            }
            _ => {}
        }
    }

    fn omp_usage(&self, o: &Value, path: &Path, st: &mut FileState, cache: &mut Cache) {
        let msg = &o["message"];
        let usage = &msg["usage"];
        if !usage.is_object() {
            return;
        }
        let mid = match o.get("id").and_then(|x| x.as_str()) {
            Some(s) => s,
            None => return,
        };
        if mid == st.msgid {
            return;
        }
        st.msgid = mid.to_string();

        let u = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let inp = u("input");
        let cread = u("cacheRead");
        // OMP отдаёт один cacheWrite — кладём в 5m-слот (pricing write_5m).
        let cw5 = u("cacheWrite");
        let out = u("output");
        // Нет токенов — не считаем пустой ход (стриминг-черновик и т.п.).
        if inp + cread + cw5 + out == 0 {
            return;
        }

        let model = msg
            .get("model")
            .and_then(|x| x.as_str())
            .or_else(|| {
                if st.model.is_empty() {
                    None
                } else {
                    Some(st.model.as_str())
                }
            })
            .unwrap_or("unknown");
        let date = match o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|t| self.date_of(t))
        {
            Some(d) => d,
            None => return,
        };
        let project = if st.proj.is_empty() {
            omp_project_fallback(path)
        } else {
            st.proj.clone()
        };

        cache.add(
            &date, "omp", model, "standard", &project, inp, cread, cw5, 0, out,
        );
        if let Some(hour) = o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|t| self.hour_of(t))
        {
            cache.add_hour_tokens(&hour, "omp", &project, inp + cread + cw5 + out);
        }
        if st.r_ts != 0 {
            st.r_in += inp;
            st.r_cread += cread;
            st.r_cw5 += cw5;
            st.r_out += out;
            st.r_model = model.to_string();
            st.r_speed = "standard".to_string();
        }
    }

    fn omp_round(&self, o: &Value, path: &Path, st: &mut FileState, cache: &mut Cache) {
        let msg = &o["message"];
        let mid = match o.get("id").and_then(|x| x.as_str()) {
            Some(s) => s,
            None => return,
        };
        // Дедуп по id user-сообщения (аналог promptId у Claude).
        if mid == st.last_pid {
            return;
        }
        let text = content_text(&msg["content"]);
        let t = text.trim();
        if t.is_empty() || t.starts_with('<') {
            return;
        }
        st.last_pid = mid.to_string();
        cache.flush_round(st, "omp", self.ring_min);
        let hour = match o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|s| self.hour_of(s))
        {
            Some(h) => h,
            None => return,
        };
        let project = if st.proj.is_empty() {
            omp_project_fallback(path)
        } else {
            st.proj.clone()
        };
        st.r_ts = o
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(parse_epoch)
            .unwrap_or(0);
        st.r_proj = project.clone();
        cache.add_round(&hour, "omp", &project);
    }
}

/// Текст user-сообщения OMP (string или content-blocks).
fn content_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .find_map(|b| {
                if b.get("type").and_then(|x| x.as_str()) == Some("text") {
                    b.get("text").and_then(|x| x.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("")
            .to_string();
    }
    String::new()
}

/// Путь вида `.../sessions/-proj-tools-telvault/….jsonl` → `/proj/tools/telvault`.
fn omp_project_fallback(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| {
            let s = n.to_string_lossy();
            if s.starts_with('-') {
                s.replacen('-', "/", 1).replace('-', "/")
            } else {
                s.replace('-', "/")
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Прочитать дописанный хвост [offset..size], вернуть его до последнего '\n'
/// и сдвинуть offset на границу строки.
fn read_tail(path: &Path, size: u64, st: &mut FileState) -> Option<String> {
    let start = st.offset.min(size); // защита от усечения/ротации
    if start >= size {
        return None;
    }
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    f.take(size - start).read_to_end(&mut buf).ok()?;
    let nl = buf.iter().rposition(|&b| b == b'\n')?;
    st.offset = start + nl as u64 + 1;
    String::from_utf8(buf[..nl].to_vec()).ok()
}

fn decode_folder(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().replace('-', "/"))
        .unwrap_or_default()
}

/// Рекурсивно собрать *.jsonl с mtime >= min_mtime.
fn walk(root: &Path, min_mtime: i64, source: Source, out: &mut Vec<(PathBuf, u64, i64, Source)>) {
    let rd = match fs::read_dir(root) {
        Ok(r) => r,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let p = e.path();
        let ft = match e.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if ft.is_dir() {
            walk(&p, min_mtime, source, out);
        } else if p.extension().map_or(false, |x| x == "jsonl") {
            if let Ok(md) = e.metadata() {
                let mt = mtime_secs(&md);
                if mt >= min_mtime {
                    out.push((p, md.len(), mt, source));
                }
            }
        }
    }
}

fn mtime_secs(md: &fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::cache::Cache;
    use std::io::Write;

    #[test]
    fn content_text_from_blocks() {
        let v: Value = serde_json::json!([
            {"type": "text", "text": "hello omp"},
            {"type": "image", "url": "x"}
        ]);
        assert_eq!(content_text(&v), "hello omp");
        assert_eq!(content_text(&Value::String("plain".into())), "plain");
    }

    #[test]
    fn omp_project_fallback_decodes_session_dir() {
        let p = Path::new("/home/u/.omp/agent/sessions/-proj-tools-telvault/s.jsonl");
        assert_eq!(omp_project_fallback(p), "/proj/tools/telvault");
    }

    #[test]
    fn scans_omp_session_usage_and_rounds() {
        let dir = std::env::temp_dir().join(format!(
            "tokmeter-omp-scan-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sess = dir.join(".omp").join("agent").join("sessions").join("-tmp");
        fs::create_dir_all(&sess).unwrap();
        let path = sess.join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"session","id":"s1","timestamp":"2026-07-08T12:00:00.000Z","cwd":"/tmp/demo"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"message","id":"u1","timestamp":"2026-07-08T12:00:01.000Z","message":{{"role":"user","content":[{{"type":"text","text":"hi"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"message","id":"a1","timestamp":"2026-07-08T12:00:02.000Z","message":{{"role":"assistant","model":"gpt-5.5","usage":{{"input":100,"output":20,"cacheRead":50,"cacheWrite":0,"totalTokens":170}}}}}}"#
        )
        .unwrap();
        // duplicate id — must not double-count
        writeln!(
            f,
            r#"{{"type":"message","id":"a1","timestamp":"2026-07-08T12:00:02.000Z","message":{{"role":"assistant","model":"gpt-5.5","usage":{{"input":100,"output":20,"cacheRead":50,"cacheWrite":0,"totalTokens":170}}}}}}"#
        )
        .unwrap();

        let mut cache = Cache::load(dir.join("cache.json"));
        let scanner = Scanner::new(dir.to_str().unwrap(), 0, 0, 0);
        scanner.update(&mut cache);

        let mut found = false;
        for (k, c) in &cache.agg {
            if k.contains("omp") && k.contains("gpt-5.5") {
                // [req, input, cache_read, cw5, cw1h, output]
                assert_eq!(c[0], 1, "req");
                assert_eq!(c[1], 100, "input");
                assert_eq!(c[2], 50, "cache_read");
                assert_eq!(c[5], 20, "output");
                assert!(k.contains("/tmp/demo"), "project in key: {k}");
                found = true;
            }
        }
        assert!(found, "omp aggregate missing: {:?}", cache.agg.keys().collect::<Vec<_>>());
        assert!(
            cache.hours.values().any(|h| h[1] >= 1),
            "expected at least one round in hours"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_inherited_codex_subagent_history() {
        let dir = std::env::temp_dir().join(format!(
            "tokmeter-codex-subagent-scan-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sessions = dir
            .join(".codex")
            .join("sessions")
            .join("2026")
            .join("07")
            .join("11");
        fs::create_dir_all(&sessions).unwrap();
        let child = sessions.join("child.jsonl");
        let mut f = fs::File::create(&child).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:00Z","type":"session_meta","payload":{{"id":"child","cwd":"/tmp/demo","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"parent"}}}}}}}}}}"#
        )
        .unwrap();

        let state = dir.join("cache.json");
        let scanner = Scanner::new(dir.to_str().unwrap(), 0, 0, 0);
        let mut cache = Cache::load(state.clone());
        scanner.update(&mut cache);
        cache.save();

        let parent = sessions.join("parent.jsonl");
        let mut p = fs::File::create(parent).unwrap();
        writeln!(
            p,
            r#"{{"timestamp":"2026-07-11T11:00:00Z","type":"session_meta","payload":{{"id":"parent","cwd":"/tmp/demo","model":"gpt-5.6-sol","source":"cli"}}}}"#
        )
        .unwrap();
        writeln!(
            p,
            r#"{{"timestamp":"2026-07-11T11:00:01Z","type":"event_msg","payload":{{"type":"user_message","message":"parent"}}}}"#
        )
        .unwrap();
        writeln!(
            p,
            r#"{{"timestamp":"2026-07-11T11:00:02Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":110}},"last_token_usage":{{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10}}}}}}}}"#
        )
        .unwrap();

        let mut f = fs::OpenOptions::new().append(true).open(child).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:00Z","type":"session_meta","payload":{{"id":"parent","cwd":"/tmp/demo","source":"cli"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:00Z","type":"event_msg","payload":{{"type":"user_message","message":"inherited"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":110}},"last_token_usage":{{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10}}}}}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:01Z","type":"inter_agent_communication_metadata","payload":{{}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:02Z","type":"turn_context","payload":{{"cwd":"/tmp/demo","model":"gpt-5.6-sol"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:03Z","type":"event_msg","payload":{{"type":"user_message","message":"child"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-11T12:00:04Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":150}},"last_token_usage":{{"input_tokens":30,"cached_input_tokens":20,"output_tokens":10}}}}}}}}"#
        )
        .unwrap();

        let mut cache = Cache::load(state);
        scanner.update(&mut cache);

        let counts = cache.agg.values().fold([0u64; 6], |mut total, counts| {
            for i in 0..6 {
                total[i] += counts[i];
            }
            total
        });
        assert_eq!(counts, [2, 30, 100, 0, 0, 20]);
        assert_eq!(cache.hours.values().map(|h| h[1]).sum::<u64>(), 2);

        let _ = fs::remove_dir_all(&dir);
    }
}

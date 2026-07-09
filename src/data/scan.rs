//! Инкрементальный разбор сессий Claude/Codex. Открываем файл только если он
//! вырос; читаем лишь дописанный хвост; учитываем каждый API-запрос один раз.

use super::cache::{Cache, FileState};
use super::limits;
use super::timeutil::{local_day, parse_epoch, ymd_hour_str, ymd_str};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct Scanner {
    claude_root: PathBuf,
    codex_root: PathBuf,
    min_mtime: i64,
    off: i64,      // локальное смещение для датирования
    ring_min: i64, // нижняя граница ts для кольца раундов (epoch)
}

impl Scanner {
    pub fn new(home: &str, min_mtime: i64, off: i64, ring_min: i64) -> Scanner {
        Scanner {
            claude_root: Path::new(home).join(".claude").join("projects"),
            codex_root: Path::new(home).join(".codex").join("sessions"),
            min_mtime,
            off,
            ring_min,
        }
    }

    /// Подхватить новые/выросшие файлы и дописать агрегаты в кэш.
    pub fn update(&self, cache: &mut Cache) {
        let mut files = Vec::new();
        walk(&self.claude_root, self.min_mtime, &mut files);
        let claude_n = files.len();
        walk(&self.codex_root, self.min_mtime, &mut files);

        for (i, (path, size, mtime)) in files.iter().enumerate() {
            let is_claude = i < claude_n;
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
                    if is_claude {
                        self.claude_line(&v, path, &mut st, cache);
                    } else {
                        self.codex_line(&v, &mut st, cache);
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
        match t {
            "session_meta" => {
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

/// Рекурсивно собрать *.jsonl с mtime >= min_mtime: (путь, размер, mtime).
fn walk(root: &Path, min_mtime: i64, out: &mut Vec<(PathBuf, u64, i64)>) {
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
            walk(&p, min_mtime, out);
        } else if p.extension().map_or(false, |x| x == "jsonl") {
            if let Ok(md) = e.metadata() {
                let mt = mtime_secs(&md);
                if mt >= min_mtime {
                    out.push((p, md.len(), mt));
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

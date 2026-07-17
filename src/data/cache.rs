//! Маленький кэш в оперативке + на диске. Хранит позицию чтения каждого файла
//! сессии (инкрементальный разбор) и компактные агрегаты по дням. Сырые события
//! на диск не пишутся.

use super::limits::{Snapshot, Window};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub const SEP: char = '\u{1f}'; // разделитель полей ключа агрегата

/// Состояние одного файла сессии для инкрементального чтения.
#[derive(Default, Clone)]
pub struct FileState {
    pub size: u64,
    pub mtime: i64,
    pub offset: u64,          // позиция после последнего полного '\n'
    pub msgid: String,        // Claude: последний учтённый message.id (дедуп)
    pub ptotal: u64,          // Codex: последний total_token_usage (монотонный)
    pub codex_parent: String, // Codex subagent: parent_thread_id до начала replay-префикса
    pub codex_replay: bool,   // Codex subagent: пропускаем унаследованную историю родителя
    pub proj: String,         // Codex: текущий cwd
    pub model: String,        // Codex: текущая модель
    pub last_pid: String,     // Claude: последний учтённый promptId (счёт раундов)
    // Аккумулятор открытого раунда (потокенная атрибуция); r_ts == 0 — раунда нет.
    pub r_ts: i64,
    pub r_proj: String,
    pub r_model: String,
    pub r_speed: String,
    pub r_in: u64,
    pub r_cread: u64,
    pub r_cw5: u64,
    pub r_cw1h: u64,
    pub r_out: u64,
}

/// Счётчики агрегата: [requests, input, cache_read, cw5, cw1h, output].
pub type Counts = [u64; 6];

/// Один завершённый раунд (пользовательский ход) с потокенной разбивкой.
/// Стоимость считается при рендере через pricing — здесь не храним.
#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    pub ts: i64,
    pub agent: String,
    pub model: String,
    pub speed: String,
    pub project: String,
    pub inp: u64,
    pub cread: u64,
    pub cw5: u64,
    pub cw1h: u64,
    pub out: u64,
}

#[derive(Clone, Default)]
pub struct CompactData {
    pub agg: HashMap<String, Counts>,
    pub hours: HashMap<String, [u64; 2]>,
    pub rounds: Vec<Round>,
    pub limits: HashMap<String, Snapshot>,
}

pub const ROUND_CAP: usize = 100;

/// Версия формата. v5: Codex subagent-сессии сохраняют модель из replay-префикса.
pub const CACHE_VERSION: u64 = 5;

pub struct Cache {
    pub files: HashMap<String, FileState>,
    pub agg: HashMap<String, Counts>, // ключ: date|agent|model|speed|project
    pub hours: HashMap<String, [u64; 2]>, // ключ: "date HH"|agent|project -> [tokens, rounds]
    pub rounds: Vec<Round>,           // кольцо последних раундов (cap ROUND_CAP)
    pub limits: HashMap<String, Snapshot>, // лимиты подписки по агентам
    pub version: u64,                 // версия загруженного с диска кэша (0 — новый)
    pub dirty: bool,
    path: PathBuf,
}

pub fn agg_key(date: &str, agent: &str, model: &str, speed: &str, project: &str) -> String {
    format!("{date}{SEP}{agent}{SEP}{model}{SEP}{speed}{SEP}{project}")
}

impl Cache {
    pub fn load(path: PathBuf) -> Cache {
        let mut c = Cache {
            files: HashMap::new(),
            agg: HashMap::new(),
            hours: HashMap::new(),
            rounds: Vec::new(),
            limits: HashMap::new(),
            version: 0,
            dirty: false,
            path,
        };
        let text = match fs::read_to_string(&c.path) {
            Ok(t) => t,
            Err(_) => return c,
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return c,
        };
        c.version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
        if let Some(files) = v.get("files").and_then(|x| x.as_object()) {
            for (k, fv) in files {
                let g = |key: &str| fv.get(key).and_then(|x| x.as_u64()).unwrap_or(0);
                let gs = |key: &str| {
                    fv.get(key)
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                c.files.insert(
                    k.clone(),
                    FileState {
                        size: g("s"),
                        mtime: fv.get("m").and_then(|x| x.as_i64()).unwrap_or(0),
                        offset: g("o"),
                        msgid: gs("id"),
                        ptotal: g("pt"),
                        codex_parent: gs("cp"),
                        codex_replay: fv.get("cr").and_then(|x| x.as_bool()).unwrap_or(false),
                        proj: gs("p"),
                        model: gs("md"),
                        last_pid: gs("lp"),
                        r_ts: fv.get("rt").and_then(|x| x.as_i64()).unwrap_or(0),
                        r_proj: gs("rp"),
                        r_model: gs("rm"),
                        r_speed: gs("rs"),
                        r_in: g("ri"),
                        r_cread: g("rc"),
                        r_cw5: g("r5"),
                        r_cw1h: g("rh"),
                        r_out: g("ro"),
                    },
                );
            }
        }
        let data = compact_from_value_tolerant(&v);
        c.agg = data.agg;
        c.hours = data.hours;
        c.rounds = data.rounds;
        c.limits = data.limits;
        c
    }

    pub fn compact_data(&self) -> CompactData {
        CompactData {
            agg: self.agg.clone(),
            hours: self.hours.clone(),
            rounds: self.rounds.clone(),
            limits: self.limits.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        date: &str,
        agent: &str,
        model: &str,
        speed: &str,
        project: &str,
        inp: u64,
        cread: u64,
        cw5: u64,
        cw1h: u64,
        out: u64,
    ) {
        let e = self
            .agg
            .entry(agg_key(date, agent, model, speed, project))
            .or_insert([0; 6]);
        e[0] += 1;
        e[1] += inp;
        e[2] += cread;
        e[3] += cw5;
        e[4] += cw1h;
        e[5] += out;
        self.dirty = true;
    }

    /// Почасово: +токены (по событию запроса).
    pub fn add_hour_tokens(&mut self, hour: &str, agent: &str, project: &str, tokens: u64) {
        let key = format!("{hour}{SEP}{agent}{SEP}{project}");
        self.hours.entry(key).or_insert([0; 2])[0] += tokens;
        self.dirty = true;
    }

    /// Почасово: +1 раунд (пользовательский ход).
    pub fn add_round(&mut self, hour: &str, agent: &str, project: &str) {
        let key = format!("{hour}{SEP}{agent}{SEP}{project}");
        self.hours.entry(key).or_insert([0; 2])[1] += 1;
        self.dirty = true;
    }

    /// Закрыть открытый раунд файла: записать его в кольцо (если ts в окне и есть
    /// токены) и обнулить аккумулятор. agent — "claude"/"codex".
    pub fn flush_round(&mut self, st: &mut FileState, agent: &str, ring_min: i64) {
        if st.r_ts != 0 {
            let sum = st.r_in + st.r_cread + st.r_cw5 + st.r_cw1h + st.r_out;
            if st.r_ts >= ring_min && sum > 0 {
                self.rounds.push(Round {
                    ts: st.r_ts,
                    agent: agent.to_string(),
                    model: st.r_model.clone(),
                    speed: st.r_speed.clone(),
                    project: st.r_proj.clone(),
                    inp: st.r_in,
                    cread: st.r_cread,
                    cw5: st.r_cw5,
                    cw1h: st.r_cw1h,
                    out: st.r_out,
                });
                if self.rounds.len() > 2 * ROUND_CAP {
                    self.rounds.sort_by(|a, b| b.ts.cmp(&a.ts));
                    self.rounds.truncate(ROUND_CAP);
                }
                self.dirty = true;
            }
        }
        st.r_ts = 0;
        st.r_proj.clear();
        st.r_model.clear();
        st.r_speed.clear();
        st.r_in = 0;
        st.r_cread = 0;
        st.r_cw5 = 0;
        st.r_cw1h = 0;
        st.r_out = 0;
    }

    /// Обновить снапшот лимитов агента, если он не старее текущего.
    pub fn set_limits(&mut self, agent: &str, snap: Snapshot) {
        if self.limits.get(agent).is_none_or(|c| snap.ts >= c.ts) {
            self.limits.insert(agent.to_string(), snap);
            self.dirty = true;
        }
    }

    /// Отметить попытку обновления лимитов (бэкофф при ошибках сети).
    pub fn touch_limits(&mut self, agent: &str, now: i64) {
        self.limits.entry(agent.to_string()).or_default().checked = now;
        self.dirty = true;
    }

    /// Разовый пересбор выбранного окна. Чистим hours/rounds полностью, дневной
    /// агрегат за окно и состояния файлов — чтобы скан перечитал их с нуля и
    /// собрал hours/rounds/agg консистентно. Старее окна — не трогаем.
    pub fn reset_recent(&mut self, date_cutoff: &str, mtime_cutoff: i64) {
        self.hours.clear();
        self.rounds.clear();
        self.agg.retain(|k, _| match k.split(SEP).next() {
            Some(d) => d < date_cutoff,
            None => false,
        });
        self.files.retain(|_, f| f.mtime < mtime_cutoff);
        self.dirty = true;
    }

    /// Раздельная подрезка: дневной агрегат до agg_cutoff, почасовой до
    /// hours_cutoff (оба — строки даты YYYY-MM-DD), состояния файлов по mtime,
    /// кольцо раундов по ts.
    pub fn prune(
        &mut self,
        agg_cutoff: &str,
        hours_cutoff: &str,
        files_mtime_cutoff: i64,
        rounds_min_ts: i64,
    ) {
        let before = self.agg.len() + self.hours.len() + self.files.len() + self.rounds.len();
        self.agg.retain(|k, _v| match k.split(SEP).next() {
            Some(date) => date >= agg_cutoff,
            None => false,
        });
        // ключ часов начинается с "YYYY-MM-DD HH..." => дата = первые 10 символов
        self.hours
            .retain(|k, _v| k.len() >= 10 && &k[..10] >= hours_cutoff);
        self.files.retain(|_k, f| f.mtime >= files_mtime_cutoff);
        self.rounds.retain(|r| r.ts >= rounds_min_ts);
        self.rounds.sort_by(|a, b| b.ts.cmp(&a.ts));
        self.rounds.truncate(ROUND_CAP);
        if self.agg.len() + self.hours.len() + self.files.len() + self.rounds.len() != before {
            self.dirty = true;
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let mut files = Map::new();
        for (k, f) in &self.files {
            let mut o = Map::new();
            o.insert("s".into(), f.size.into());
            o.insert("m".into(), f.mtime.into());
            o.insert("o".into(), f.offset.into());
            if !f.msgid.is_empty() {
                o.insert("id".into(), f.msgid.clone().into());
            }
            if f.ptotal != 0 {
                o.insert("pt".into(), f.ptotal.into());
            }
            if !f.codex_parent.is_empty() {
                o.insert("cp".into(), f.codex_parent.clone().into());
            }
            if f.codex_replay {
                o.insert("cr".into(), true.into());
            }
            if !f.proj.is_empty() {
                o.insert("p".into(), f.proj.clone().into());
            }
            if !f.model.is_empty() {
                o.insert("md".into(), f.model.clone().into());
            }
            if !f.last_pid.is_empty() {
                o.insert("lp".into(), f.last_pid.clone().into());
            }
            if f.r_ts != 0 {
                o.insert("rt".into(), f.r_ts.into());
            }
            if !f.r_proj.is_empty() {
                o.insert("rp".into(), f.r_proj.clone().into());
            }
            if !f.r_model.is_empty() {
                o.insert("rm".into(), f.r_model.clone().into());
            }
            if !f.r_speed.is_empty() {
                o.insert("rs".into(), f.r_speed.clone().into());
            }
            if f.r_in != 0 {
                o.insert("ri".into(), f.r_in.into());
            }
            if f.r_cread != 0 {
                o.insert("rc".into(), f.r_cread.into());
            }
            if f.r_cw5 != 0 {
                o.insert("r5".into(), f.r_cw5.into());
            }
            if f.r_cw1h != 0 {
                o.insert("rh".into(), f.r_cw1h.into());
            }
            if f.r_out != 0 {
                o.insert("ro".into(), f.r_out.into());
            }
            files.insert(k.clone(), Value::Object(o));
        }
        let mut root = compact_to_value(&self.compact_data())
            .as_object()
            .cloned()
            .unwrap_or_default();
        root.insert("version".into(), CACHE_VERSION.into());
        root.insert("files".into(), Value::Object(files));

        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(&Value::Object(root)) {
            let tmp = self.path.with_extension("json.tmp");
            if fs::write(&tmp, text).is_ok() {
                let _ = fs::rename(&tmp, &self.path);
            }
        }
        self.dirty = false;
    }
}

pub fn compact_to_value(data: &CompactData) -> Value {
    let agg = data
        .agg
        .iter()
        .map(|(key, counts)| {
            (
                key.clone(),
                Value::Array(counts.iter().map(|value| (*value).into()).collect()),
            )
        })
        .collect();
    let hours = data
        .hours
        .iter()
        .map(|(key, counts)| {
            (
                key.clone(),
                Value::Array(counts.iter().map(|value| (*value).into()).collect()),
            )
        })
        .collect();
    let rounds = data
        .rounds
        .iter()
        .map(|round| {
            Value::Array(vec![
                round.ts.into(),
                round.agent.clone().into(),
                round.model.clone().into(),
                round.speed.clone().into(),
                round.project.clone().into(),
                round.inp.into(),
                round.cread.into(),
                round.cw5.into(),
                round.cw1h.into(),
                round.out.into(),
            ])
        })
        .collect();
    let limits = data
        .limits
        .iter()
        .map(|(agent, snapshot)| {
            let windows = snapshot
                .windows
                .iter()
                .map(|window| {
                    Value::Array(vec![
                        window.label.clone().into(),
                        window.pct.into(),
                        window.resets.into(),
                    ])
                })
                .collect();
            let mut value = Map::new();
            value.insert("ts".into(), snapshot.ts.into());
            value.insert("ck".into(), snapshot.checked.into());
            value.insert("w".into(), Value::Array(windows));
            (agent.clone(), Value::Object(value))
        })
        .collect();

    let mut root = Map::new();
    root.insert("agg".into(), Value::Object(agg));
    root.insert("hours".into(), Value::Object(hours));
    root.insert("rounds".into(), Value::Array(rounds));
    root.insert("limits".into(), Value::Object(limits));
    Value::Object(root)
}

pub fn compact_from_value(value: &Value) -> Result<CompactData, String> {
    decode_compact(value, true)
}

fn compact_from_value_tolerant(value: &Value) -> CompactData {
    decode_compact(value, false).unwrap_or_default()
}

fn decode_compact(value: &Value, strict: bool) -> Result<CompactData, String> {
    let root = value
        .as_object()
        .ok_or_else(|| "compact data must be an object".to_string())?;
    let mut data = CompactData::default();

    match root.get("agg").and_then(Value::as_object) {
        Some(values) => {
            for (key, value) in values {
                let Some(array) = value.as_array() else {
                    if strict {
                        return Err(format!("invalid aggregate {key}"));
                    }
                    continue;
                };
                if array.len() != 6 || (strict && key.split(SEP).count() != 5) {
                    if strict {
                        return Err(format!("invalid aggregate {key}"));
                    }
                    continue;
                }
                let Some(counts) = array_u64::<6>(array) else {
                    if strict {
                        return Err(format!("invalid aggregate {key}"));
                    }
                    continue;
                };
                data.agg.insert(key.clone(), counts);
            }
        }
        None if strict => return Err("missing agg".to_string()),
        None => {}
    }

    match root.get("hours").and_then(Value::as_object) {
        Some(values) => {
            for (key, value) in values {
                let Some(array) = value.as_array() else {
                    if strict {
                        return Err(format!("invalid hour aggregate {key}"));
                    }
                    continue;
                };
                if array.len() != 2 || (strict && key.split(SEP).count() != 3) {
                    if strict {
                        return Err(format!("invalid hour aggregate {key}"));
                    }
                    continue;
                }
                let Some(counts) = array_u64::<2>(array) else {
                    if strict {
                        return Err(format!("invalid hour aggregate {key}"));
                    }
                    continue;
                };
                data.hours.insert(key.clone(), counts);
            }
        }
        None if strict => return Err("missing hours".to_string()),
        None => {}
    }

    match root.get("rounds").and_then(Value::as_array) {
        Some(values) => {
            for value in values {
                let Some(array) = value.as_array() else {
                    if strict {
                        return Err("invalid round".to_string());
                    }
                    continue;
                };
                let round = decode_round(array);
                match round {
                    Some(round) if array.len() == 10 => data.rounds.push(round),
                    _ if strict => return Err("invalid round".to_string()),
                    _ => {}
                }
            }
        }
        None if strict => return Err("missing rounds".to_string()),
        None => {}
    }

    match root.get("limits").and_then(Value::as_object) {
        Some(values) => {
            for (agent, value) in values {
                let snapshot = decode_snapshot(value, strict);
                match snapshot {
                    Ok(Some(snapshot)) => {
                        data.limits.insert(agent.clone(), snapshot);
                    }
                    Ok(None) => {}
                    Err(error) => return Err(format!("invalid limit {agent}: {error}")),
                }
            }
        }
        None if strict => return Err("missing limits".to_string()),
        None => {}
    }

    Ok(data)
}

fn array_u64<const N: usize>(array: &[Value]) -> Option<[u64; N]> {
    if array.len() != N {
        return None;
    }
    let mut values = [0; N];
    for (index, value) in array.iter().enumerate() {
        values[index] = value.as_u64()?;
    }
    Some(values)
}

fn decode_round(array: &[Value]) -> Option<Round> {
    Some(Round {
        ts: array.first()?.as_i64()?,
        agent: array.get(1)?.as_str()?.to_string(),
        model: array.get(2)?.as_str()?.to_string(),
        speed: array.get(3)?.as_str()?.to_string(),
        project: array.get(4)?.as_str()?.to_string(),
        inp: array.get(5)?.as_u64()?,
        cread: array.get(6)?.as_u64()?,
        cw5: array.get(7)?.as_u64()?,
        cw1h: array.get(8)?.as_u64()?,
        out: array.get(9)?.as_u64()?,
    })
}

fn decode_snapshot(value: &Value, strict: bool) -> Result<Option<Snapshot>, String> {
    let Some(value) = value.as_object() else {
        return if strict {
            Err("snapshot must be an object".to_string())
        } else {
            Ok(None)
        };
    };
    let ts = value.get("ts").and_then(Value::as_i64).unwrap_or(0);
    let checked = value.get("ck").and_then(Value::as_i64).unwrap_or(0);
    if strict && (ts < 0 || checked < 0) {
        return Err("negative timestamp".to_string());
    }
    let Some(windows) = value.get("w").and_then(Value::as_array) else {
        return if strict {
            Err("missing windows".to_string())
        } else {
            Ok(Some(Snapshot {
                ts,
                checked,
                windows: Vec::new(),
            }))
        };
    };
    let mut decoded = Vec::new();
    for window in windows {
        let Some(array) = window.as_array() else {
            if strict {
                return Err("window must be an array".to_string());
            }
            continue;
        };
        let parsed = (|| {
            if array.len() != 3 {
                return None;
            }
            let pct = array.get(1)?.as_f64()?;
            if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
                return None;
            }
            Some(Window {
                label: array.first()?.as_str()?.to_string(),
                pct,
                resets: array.get(2)?.as_i64()?,
            })
        })();
        match parsed {
            Some(window) => decoded.push(window),
            None if strict => return Err("invalid window".to_string()),
            None => {}
        }
    }
    Ok(Some(Snapshot {
        ts,
        checked,
        windows: decoded,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn sample_data() -> CompactData {
        let mut data = CompactData::default();
        data.agg.insert(
            agg_key("2026-07-17", "claude", "opus", "standard", "/proj"),
            [1, 2, 3, 4, 5, 6],
        );
        data.hours
            .insert(format!("2026-07-17 19{SEP}claude{SEP}/proj"), [20, 1]);
        data.rounds.push(Round {
            ts: 1_784_317_200,
            agent: "claude".into(),
            model: "opus".into(),
            speed: "standard".into(),
            project: "/proj".into(),
            inp: 2,
            cread: 3,
            cw5: 4,
            cw1h: 5,
            out: 6,
        });
        data.limits.insert(
            "claude".into(),
            Snapshot {
                ts: 1_784_317_200,
                checked: 1_784_317_200,
                windows: vec![Window {
                    label: "5h".into(),
                    pct: 42.0,
                    resets: 1_784_320_000,
                }],
            },
        );
        data
    }

    fn temp_path() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "tokmeter-cache-test-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn compact_round_trip_excludes_files() {
        let data = sample_data();
        let value = compact_to_value(&data);
        assert!(value.get("files").is_none());
        let decoded = compact_from_value(&value).unwrap();
        assert_eq!(decoded.agg, data.agg);
        assert_eq!(decoded.hours, data.hours);
        assert_eq!(decoded.rounds, data.rounds);
        assert_eq!(decoded.limits["claude"].windows[0].label, "5h");
    }

    #[test]
    fn local_cache_schema_round_trips_with_files() {
        let path = temp_path();
        let data = sample_data();
        let mut cache = Cache {
            files: HashMap::from([("/session.jsonl".into(), FileState::default())]),
            agg: data.agg.clone(),
            hours: data.hours.clone(),
            rounds: data.rounds.clone(),
            limits: data.limits.clone(),
            version: CACHE_VERSION,
            dirty: true,
            path: path.clone(),
        };
        cache.save();
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["version"].as_u64(), Some(CACHE_VERSION));
        assert!(value["files"].get("/session.jsonl").is_some());
        let loaded = Cache::load(path.clone());
        assert_eq!(loaded.agg, data.agg);
        assert_eq!(loaded.rounds, data.rounds);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn strict_decoder_rejects_malformed_shapes() {
        for value in [
            serde_json::json!({"agg": [], "hours": {}, "rounds": [], "limits": {}}),
            serde_json::json!({"agg": {"bad": [1,2]}, "hours": {}, "rounds": [], "limits": {}}),
            serde_json::json!({"agg": {}, "hours": {}, "rounds": [[1]], "limits": {}}),
            serde_json::json!({"agg": {}, "hours": {}, "rounds": [], "limits": {"claude": {"w": [["5h", 120, 0]]}}}),
        ] {
            assert!(compact_from_value(&value).is_err());
        }
    }
}

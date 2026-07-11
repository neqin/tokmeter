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
#[derive(Clone)]
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

pub const ROUND_CAP: usize = 100;

/// Версия формата. v4: недавние агрегаты пересобираются без унаследованной
/// истории Codex subagent-сессий.
pub const CACHE_VERSION: u64 = 4;

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
        if let Some(agg) = v.get("agg").and_then(|x| x.as_object()) {
            for (k, av) in agg {
                if let Some(arr) = av.as_array() {
                    let mut c6 = [0u64; 6];
                    for i in 0..6 {
                        c6[i] = arr.get(i).and_then(|x| x.as_u64()).unwrap_or(0);
                    }
                    c.agg.insert(k.clone(), c6);
                }
            }
        }
        if let Some(hours) = v.get("hours").and_then(|x| x.as_object()) {
            for (k, av) in hours {
                if let Some(arr) = av.as_array() {
                    c.hours.insert(
                        k.clone(),
                        [
                            arr.first().and_then(|x| x.as_u64()).unwrap_or(0),
                            arr.get(1).and_then(|x| x.as_u64()).unwrap_or(0),
                        ],
                    );
                }
            }
        }
        if let Some(limits) = v.get("limits").and_then(|x| x.as_object()) {
            for (agent, lv) in limits {
                let windows: Vec<Window> = lv
                    .get("w")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|w| {
                                let a = w.as_array()?;
                                Some(Window {
                                    label: a.first()?.as_str()?.to_string(),
                                    pct: a.get(1)?.as_f64()?,
                                    resets: a.get(2).and_then(|x| x.as_i64()).unwrap_or(0),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                c.limits.insert(
                    agent.clone(),
                    Snapshot {
                        ts: lv.get("ts").and_then(|x| x.as_i64()).unwrap_or(0),
                        checked: lv.get("ck").and_then(|x| x.as_i64()).unwrap_or(0),
                        windows,
                    },
                );
            }
        }
        if let Some(rounds) = v.get("rounds").and_then(|x| x.as_array()) {
            for rv in rounds {
                if let Some(a) = rv.as_array() {
                    let gi = |i: usize| a.get(i).and_then(|x| x.as_i64()).unwrap_or(0);
                    let gu = |i: usize| a.get(i).and_then(|x| x.as_u64()).unwrap_or(0);
                    let gstr =
                        |i: usize| a.get(i).and_then(|x| x.as_str()).unwrap_or("").to_string();
                    c.rounds.push(Round {
                        ts: gi(0),
                        agent: gstr(1),
                        model: gstr(2),
                        speed: gstr(3),
                        project: gstr(4),
                        inp: gu(5),
                        cread: gu(6),
                        cw5: gu(7),
                        cw1h: gu(8),
                        out: gu(9),
                    });
                }
            }
        }
        c
    }

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

    /// Разовый пересбор недавнего окна (апгрейд со старых кэшей с неполным
    /// почасовым агрегатом). Чистим hours/rounds полностью, дневной агрегат за
    /// окно и состояния недавних файлов — чтобы скан перечитал их с нуля и
    /// собрал hours/rounds/agg-recent консистентно. Старее окна — не трогаем.
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
        let mut agg = Map::new();
        for (k, c) in &self.agg {
            agg.insert(
                k.clone(),
                Value::Array(c.iter().map(|n| (*n).into()).collect()),
            );
        }
        let mut hours = Map::new();
        for (k, c) in &self.hours {
            hours.insert(
                k.clone(),
                Value::Array(c.iter().map(|n| (*n).into()).collect()),
            );
        }
        let rounds: Vec<Value> = self
            .rounds
            .iter()
            .map(|r| {
                Value::Array(vec![
                    r.ts.into(),
                    r.agent.clone().into(),
                    r.model.clone().into(),
                    r.speed.clone().into(),
                    r.project.clone().into(),
                    r.inp.into(),
                    r.cread.into(),
                    r.cw5.into(),
                    r.cw1h.into(),
                    r.out.into(),
                ])
            })
            .collect();
        let mut limits = Map::new();
        for (agent, s) in &self.limits {
            let mut o = Map::new();
            o.insert("ts".into(), s.ts.into());
            o.insert("ck".into(), s.checked.into());
            let w: Vec<Value> = s
                .windows
                .iter()
                .map(|w| Value::Array(vec![w.label.clone().into(), w.pct.into(), w.resets.into()]))
                .collect();
            o.insert("w".into(), Value::Array(w));
            limits.insert(agent.clone(), Value::Object(o));
        }
        let mut root = Map::new();
        root.insert("version".into(), CACHE_VERSION.into());
        root.insert("files".into(), Value::Object(files));
        root.insert("agg".into(), Value::Object(agg));
        root.insert("hours".into(), Value::Object(hours));
        root.insert("rounds".into(), Value::Array(rounds));
        root.insert("limits".into(), Value::Object(limits));

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

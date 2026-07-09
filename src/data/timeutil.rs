//! Время без внешних крейтов: UTC-таймстемпы сессий -> epoch, локальная дата
//! через один FFI-вызов `localtime_r` (учитываем смещение/DST как Python-версия).

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Смещение локального времени от UTC в секундах (одним вызовом localtime_r).
pub fn local_offset(now: i64) -> i64 {
    unsafe {
        let t: libc::time_t = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        let y = tm.tm_year as i64 + 1900;
        let m = tm.tm_mon as i64 + 1;
        let d = tm.tm_mday as i64;
        let as_utc = days_from_civil(y, m, d) * 86400
            + tm.tm_hour as i64 * 3600
            + tm.tm_min as i64 * 60
            + tm.tm_sec as i64;
        as_utc - now
    }
}

/// Локальный «день» (число дней от эпохи) для epoch с заданным смещением.
pub fn local_day(epoch: i64, off: i64) -> i64 {
    (epoch + off).div_euclid(86400)
}

pub fn ymd_str(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// "YYYY-MM-DD" -> число дней от эпохи (обратно к `ymd_str`).
pub fn ymd_to_days(s: &str) -> Option<i64> {
    if s.len() < 10 {
        return None;
    }
    let y = s[0..4].parse::<i64>().ok()?;
    let m = s[5..7].parse::<i64>().ok()?;
    let d = s[8..10].parse::<i64>().ok()?;
    Some(days_from_civil(y, m, d))
}

/// "HH:MM" локального времени для epoch с заданным смещением.
pub fn hm(epoch: i64, off: i64) -> String {
    let sod = (epoch + off).rem_euclid(86400);
    format!("{:02}:{:02}", sod / 3600, (sod % 3600) / 60)
}

/// HH:MM:SS локального времени.
pub fn clock(now: i64, off: i64) -> String {
    let sod = (now + off).rem_euclid(86400);
    format!("{:02}:{:02}:{:02}", sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// Локальный час как "YYYY-MM-DD HH" (ключ почасового агрегата).
pub fn ymd_hour_str(epoch: i64, off: i64) -> String {
    let local = epoch + off;
    let (y, m, d) = civil_from_days(local.div_euclid(86400));
    let h = local.rem_euclid(86400) / 3600;
    format!("{:04}-{:02}-{:02} {:02}", y, m, d, h)
}

/// Сколько секунд прошло с локальной полуночи (для среднего за день).
pub fn secs_into_local_day(now: i64, off: i64) -> i64 {
    (now + off).rem_euclid(86400)
}

/// Разбор ISO-таймстемпа сессии в epoch (UTC). Поддержка `...Z`, `±hh:mm`,
/// дробных секунд. Возвращает None, если формат неожиданный.
pub fn parse_epoch(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let y = num(&ts[0..4])?;
    let mo = num(&ts[5..7])?;
    let d = num(&ts[8..10])?;
    let h = num(&ts[11..13])?;
    let mi = num(&ts[14..16])?;
    let s = num(&ts[17..19])?;
    let mut e = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s;

    // хвост после секунд: пропускаем дробную часть, ищем смещение зоны
    let mut i = 19;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') && i + 6 <= b.len() {
        let sign = if b[i] == b'+' { 1 } else { -1 };
        let oh = num(&ts[i + 1..i + 3]).unwrap_or(0);
        let om = num(&ts[i + 4..i + 6]).unwrap_or(0);
        e -= sign * (oh * 3600 + om * 60); // привести к UTC
    }
    Some(e)
}

// --- алгоритмы Говарда Хиннанта (days <-> civil), всё в i64 ---

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

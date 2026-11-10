//! System.DateTime, DateTimeOffset, TimeSpan and TimeZoneInfo.
//!
//! Values are kept as .NET ticks (100 ns since 0001-01-01). The service tier's local time zone is
//! UTC (as in BC online), so Local and Utc clock times are the same. AL DateTime values are UTC
//! instants; converted to .NET they become DateTime values of kind Utc.

use super::culture::{self, Culture};
use super::*;
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

pub const TICKS_PER_MS: i64 = 10_000;
pub const TICKS_PER_SEC: i64 = 10_000_000;
pub const TICKS_PER_DAY: i64 = 864_000_000_000;
/// Ticks of 1970-01-01T00:00:00.
pub const EPOCH_TICKS: i64 = 621_355_968_000_000_000;
pub const MAX_TICKS: i64 = 3_155_378_975_999_999_999;

pub fn ms_to_ticks(ms: i64) -> i64 {
    ms * TICKS_PER_MS + EPOCH_TICKS
}

pub fn ticks_to_ms(t: i64) -> i64 {
    (t - EPOCH_TICKS).div_euclid(TICKS_PER_MS)
}

pub fn ticks_to_naive(t: i64) -> NaiveDateTime {
    let rel = t - EPOCH_TICKS;
    let secs = rel.div_euclid(TICKS_PER_SEC);
    let nanos = (rel.rem_euclid(TICKS_PER_SEC) * 100) as u32;
    chrono::DateTime::from_timestamp(secs, nanos).map(|d| d.naive_utc()).unwrap_or_default()
}

pub fn naive_to_ticks(n: &NaiveDateTime) -> i64 {
    let u = n.and_utc();
    u.timestamp() * TICKS_PER_SEC + (u.timestamp_subsec_nanos() / 100) as i64 + EPOCH_TICKS
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DtKind {
    Unspecified = 0,
    Utc = 1,
    Local = 2,
}

#[derive(Clone, Copy, Debug)]
pub struct NetDateTime {
    pub ticks: i64,
    pub kind: DtKind,
}

impl NetDateTime {
    pub fn from_al(ms: i64) -> NetDateTime {
        if ms == bc_store::DATETIME_UNDEFINED {
            return NetDateTime { ticks: 0, kind: DtKind::Utc };
        }
        NetDateTime { ticks: ms_to_ticks(ms), kind: DtKind::Utc }
    }
    /// The AL DateTime (UTC). Local and Unspecified clock times are UTC on this service tier.
    pub fn to_al(&self) -> i64 {
        if self.ticks == 0 {
            return bc_store::DATETIME_UNDEFINED;
        }
        ticks_to_ms(self.ticks)
    }
    pub fn format(&self, fmt: &str, c: &Culture) -> String {
        format_ticks(self.ticks, Some(self.kind), None, fmt, c)
    }
}

fn dt(ticks: i64, kind: DtKind) -> Val {
    net(NetObj::DateTime(NetDateTime { ticks: ticks.clamp(0, MAX_TICKS), kind }))
}

pub fn timespan(ticks: i64) -> Val {
    net(NetObj::TimeSpan(ticks))
}

fn dto(utc: i64, off: i32) -> Val {
    net(NetObj::DateTimeOffset(utc, off))
}

fn now_ticks() -> i64 {
    ms_to_ticks(crate::clock::now_ms(None))
}

/// Ticks of a DateTime-like argument (AL DateTime/Date or a .NET DateTime/DateTimeOffset).
pub fn arg_dt(v: Option<&Val>) -> Option<NetDateTime> {
    match v? {
        Val::DateTime(ms) => Some(NetDateTime::from_al(*ms)),
        Val::Date(d) => crate::value::date_to_naive(*d).map(|n| NetDateTime { ticks: naive_to_ticks(&n.and_hms_opt(0, 0, 0).unwrap()), kind: DtKind::Unspecified }),
        Val::Net(n) => match &*n.borrow() {
            NetObj::DateTime(d) => Some(*d),
            NetObj::DateTimeOffset(u, off) => Some(NetDateTime { ticks: u + *off as i64 * 60 * TICKS_PER_SEC, kind: DtKind::Unspecified }),
            NetObj::Prim(_, Val::DateTime(ms)) => Some(NetDateTime::from_al(*ms)),
            NetObj::Prim(_, Val::Text(s)) => parse_any(s, culture::current()).map(|p| p.datetime()),
            _ => None,
        },
        Val::Text(s) => parse_any(s, culture::current()).map(|p| p.datetime()),
        _ => None,
    }
}

/// Ticks of a TimeSpan-like argument (AL Duration in ms, .NET TimeSpan, numbers as ticks).
pub fn arg_ts(v: Option<&Val>) -> i64 {
    match v {
        Some(Val::Duration(ms)) => ms * TICKS_PER_MS,
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::TimeSpan(t) => *t,
            NetObj::Prim(_, Val::Duration(ms)) => ms * TICKS_PER_MS,
            _ => 0,
        },
        Some(v) => v.as_i64(),
        None => 0,
    }
}

fn provider(a: &Args, i: usize) -> &'static Culture {
    match a.raw(i) {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Culture(c, _) => c,
            _ => culture::current(),
        },
        _ => culture::current(),
    }
}

fn add_months(t: i64, months: i64) -> i64 {
    let n = ticks_to_naive(t);
    let total = n.year() as i64 * 12 + n.month0() as i64 + months;
    let (y, m) = (total.div_euclid(12) as i32, total.rem_euclid(12) as u32 + 1);
    let mut d = n.day();
    let date = loop {
        if let Some(x) = NaiveDate::from_ymd_opt(y, m, d) {
            break x;
        }
        d -= 1;
    };
    naive_to_ticks(&date.and_time(n.time()))
}

fn f64_ticks(x: f64, unit: i64) -> i64 {
    // .NET rounds AddDays/AddHours/... to the nearest millisecond
    let ms = (x * (unit / TICKS_PER_MS) as f64).round() as i64;
    ms * TICKS_PER_MS
}

fn dec(x: f64) -> Val {
    Val::Dec(rust_decimal::Decimal::from_f64_retain(x).map(|d| d.round_dp(12).normalize()).unwrap_or_default())
}

pub fn call(s: &mut Session, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    match ty {
        "System.DateTime" => datetime_call(s, f, n, m, a),
        "System.DateTimeOffset" => dto_call(s, f, n, m, a),
        "System.TimeSpan" => timespan_call(s, f, n, m, a),
        "System.TimeZoneInfo" => tz_call(n, m, a),
        "System.DateTimeKind" => ok(match m {
            "unspecified" => enum_val(ty, 0),
            "utc" => enum_val(ty, 1),
            "local" => enum_val(ty, 2),
            _ => return Ok(Out::Unknown),
        }),
        "System.DayOfWeek" => ok(match DAYS.iter().position(|d| d.eq_ignore_ascii_case(m)) {
            Some(i) => enum_val(ty, i as i64),
            None => return Ok(Out::Unknown),
        }),
        _ => Ok(Out::Unknown),
    }
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    let ints: Vec<i64> = (0..a.len()).map(|i| a.int(i)).collect();
    match ty {
        "System.DateTime" => {
            if a.len() == 1 {
                return Ok(dt(a.int(0), DtKind::Unspecified));
            }
            if a.len() == 2 {
                return Ok(dt(a.int(0), kind_of(a.int(1))));
            }
            let (y, mo, d) = (ints[0] as i32, ints.get(1).copied().unwrap_or(1) as u32, ints.get(2).copied().unwrap_or(1) as u32);
            let (h, mi, sec) = (ints.get(3).copied().unwrap_or(0) as u32, ints.get(4).copied().unwrap_or(0) as u32, ints.get(5).copied().unwrap_or(0) as u32);
            let ms = if a.len() >= 7 { ints[6] } else { 0 };
            let kind = if a.len() == 8 { kind_of(ints[7]) } else if a.len() == 7 && matches!(a.raw(6), Some(Val::Net(_))) { kind_of(ints[6]) } else { DtKind::Unspecified };
            let ms = if a.len() == 7 && matches!(a.raw(6), Some(Val::Net(_))) { 0 } else { ms };
            match NaiveDate::from_ymd_opt(y, mo, d).and_then(|x| x.and_hms_opt(h, mi, sec)) {
                Some(n) => Ok(dt(naive_to_ticks(&n) + ms * TICKS_PER_MS, kind)),
                None => net_err("DateTime", "DateTime", "Year, Month, and Day parameters describe an un-representable DateTime."),
            }
        }
        "System.DateTimeOffset" => {
            match a.len() {
                0 => Ok(dto(0, 0)),
                1 => {
                    let d = arg_dt(a.raw(0)).unwrap_or(NetDateTime { ticks: 0, kind: DtKind::Utc });
                    Ok(dto(d.ticks, 0))
                }
                2 => {
                    let off = (arg_ts(a.raw(1)) / (60 * TICKS_PER_SEC)) as i32;
                    let clock = match a.raw(0) {
                        Some(Val::Int(_) | Val::BigInt(_)) => a.int(0),
                        v => arg_dt(v).map(|d| d.ticks).unwrap_or(0),
                    };
                    Ok(dto(clock - off as i64 * 60 * TICKS_PER_SEC, off))
                }
                _ => {
                    let off = (arg_ts(a.raw(a.len() - 1)) / (60 * TICKS_PER_SEC)) as i32;
                    let ms = if a.len() >= 8 { ints[6] } else { 0 };
                    match NaiveDate::from_ymd_opt(ints[0] as i32, ints[1] as u32, ints[2] as u32).and_then(|x| x.and_hms_opt(ints[3] as u32, ints[4] as u32, ints[5] as u32)) {
                        Some(nd) => Ok(dto(naive_to_ticks(&nd) + ms * TICKS_PER_MS - off as i64 * 60 * TICKS_PER_SEC, off)),
                        None => net_err("DateTimeOffset", "DateTimeOffset", "Year, Month, and Day parameters describe an un-representable DateTime."),
                    }
                }
            }
        }
        _ => {
            // TimeSpan(ticks) / (h, m, s) / (d, h, m, s) / (d, h, m, s, ms)
            let t = match ints.len() {
                0 => 0,
                1 => ints[0],
                3 => ((ints[0] * 60 + ints[1]) * 60 + ints[2]) * TICKS_PER_SEC,
                4 => (((ints[0] * 24 + ints[1]) * 60 + ints[2]) * 60 + ints[3]) * TICKS_PER_SEC,
                _ => (((ints[0] * 24 + ints[1]) * 60 + ints[2]) * 60 + ints[3]) * TICKS_PER_SEC + ints[4] * TICKS_PER_MS,
            };
            Ok(timespan(t))
        }
    }
}

fn kind_of(i: i64) -> DtKind {
    match i {
        1 => DtKind::Utc,
        2 => DtKind::Local,
        _ => DtKind::Unspecified,
    }
}

pub const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn datetime_call(s: &mut Session, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
    // static members
    match m {
        "now" => return ok(dt(now_ticks(), DtKind::Local)),
        "utcnow" => return ok(dt(now_ticks(), DtKind::Utc)),
        "today" => {
            let t = now_ticks();
            return ok(dt(t - t.rem_euclid(TICKS_PER_DAY), DtKind::Local));
        }
        "minvalue" => return ok(dt(0, DtKind::Unspecified)),
        "maxvalue" => return ok(dt(MAX_TICKS, DtKind::Unspecified)),
        "daysinmonth" => {
            let (y, mo) = (a.int(0) as i32, a.int(1) as u32);
            let first = NaiveDate::from_ymd_opt(y, mo, 1);
            let next = if mo == 12 { NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { NaiveDate::from_ymd_opt(y, mo + 1, 1) };
            return ok(Val::Int(match (first, next) {
                (Some(a), Some(b)) => (b - a).num_days() as i32,
                _ => 0,
            }));
        }
        "isleapyear" => {
            let y = a.int(0) as i32;
            return ok(Val::Bool((y % 4 == 0 && y % 100 != 0) || y % 400 == 0));
        }
        "fromoadate" => {
            let d = a.get(0).as_dec().to_string().parse::<f64>().unwrap_or(0.0);
            let base = naive_to_ticks(&NaiveDate::from_ymd_opt(1899, 12, 30).unwrap().and_hms_opt(0, 0, 0).unwrap());
            let days = d.trunc();
            let frac = (d - days).abs();
            let t = base + days as i64 * TICKS_PER_DAY + (frac * TICKS_PER_DAY as f64 / TICKS_PER_MS as f64).round() as i64 * TICKS_PER_MS;
            return ok(dt(t, DtKind::Unspecified));
        }
        "specifykind" => {
            let d = arg_dt(a.raw(0)).unwrap_or(NetDateTime { ticks: 0, kind: DtKind::Unspecified });
            return ok(dt(d.ticks, kind_of(a.int(1))));
        }
        "compare" => {
            let (x, y) = (arg_dt(a.raw(0)).map(|d| d.ticks).unwrap_or(0), arg_dt(a.raw(1)).map(|d| d.ticks).unwrap_or(0));
            return ok(Val::Int(x.cmp(&y) as i32));
        }
        "parse" | "tryparse" | "parseexact" | "tryparseexact" => {
            let text = a.text(0);
            let exact = m.ends_with("exact");
            let fmts: Vec<String> = if exact {
                match a.raw(1) {
                    Some(Val::Net(x)) => match &*x.borrow() {
                        NetObj::Array(_, v) => v.iter().map(|v| v.to_text()).collect(),
                        _ => vec![a.text(1)],
                    },
                    _ => vec![a.text(1)],
                }
            } else {
                vec![]
            };
            let c = provider(a, if exact { 2 } else { 1 });
            let parsed = if exact { fmts.iter().find_map(|fm| parse_exact(text.trim(), fm, c)) } else { parse_any(&text, c) };
            let roundtrip = (0..a.len()).any(|i| matches!(a.raw(i), Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Prim("System.Globalization.DateTimeStyles", v) if v.as_i64() & 128 != 0)));
            let adjust_utc = (0..a.len()).any(|i| matches!(a.raw(i), Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Prim("System.Globalization.DateTimeStyles", v) if v.as_i64() & 16 != 0)));
            let v = parsed.map(|p| {
                let d = p.datetime();
                match p.offset {
                    // a parsed offset converts to local time (UTC here) unless RoundtripKind/AdjustToUniversal
                    Some(_) if roundtrip || adjust_utc => dt(p.utc_ticks(), DtKind::Utc),
                    Some(_) => dt(p.utc_ticks(), if p.zulu && !roundtrip { DtKind::Local } else { DtKind::Local }),
                    None => dt(d.ticks, if adjust_utc { DtKind::Utc } else { DtKind::Unspecified }),
                }
            });
            if m.starts_with("try") {
                let ok_ = v.is_some();
                s.net_out(f, a, a.len() - 1, v.unwrap_or_else(|| dt(0, DtKind::Unspecified)))?;
                return ok(Val::Bool(ok_));
            }
            return match v {
                Some(v) => ok(v),
                None => net_err("DateTime", "Parse", format!("String '{text}' was not recognized as a valid DateTime.")),
            };
        }
        _ => {}
    }
    let d = match &*n.borrow() {
        NetObj::DateTime(d) => *d,
        NetObj::Null(_) => return Ok(Out::Unknown),
        _ => return Ok(Out::Unknown),
    };
    let nd = ticks_to_naive(d.ticks);
    let c = culture::current();
    ok(match m {
        "year" => Val::Int(nd.year()),
        "month" => Val::Int(nd.month() as i32),
        "day" => Val::Int(nd.day() as i32),
        "hour" => Val::Int(nd.hour() as i32),
        "minute" => Val::Int(nd.minute() as i32),
        "second" => Val::Int(nd.second() as i32),
        "millisecond" => Val::Int((d.ticks.rem_euclid(TICKS_PER_SEC) / TICKS_PER_MS) as i32),
        "ticks" => Val::BigInt(d.ticks),
        "dayofweek" => enum_val("System.DayOfWeek", nd.weekday().num_days_from_sunday() as i64),
        "dayofyear" => Val::Int(nd.ordinal() as i32),
        "date" => dt(d.ticks - d.ticks.rem_euclid(TICKS_PER_DAY), d.kind),
        "timeofday" => timespan(d.ticks.rem_euclid(TICKS_PER_DAY)),
        "kind" => enum_val("System.DateTimeKind", d.kind as i64),
        "touniversaltime" => dt(d.ticks, DtKind::Utc),
        "tolocaltime" => dt(d.ticks, DtKind::Local),
        "isdaylightsavingtime" => Val::Bool(false),
        "adddays" => dt(d.ticks + f64_ticks(arg_f64(a, 0), TICKS_PER_DAY), d.kind),
        "addhours" => dt(d.ticks + f64_ticks(arg_f64(a, 0), 3600 * TICKS_PER_SEC), d.kind),
        "addminutes" => dt(d.ticks + f64_ticks(arg_f64(a, 0), 60 * TICKS_PER_SEC), d.kind),
        "addseconds" => dt(d.ticks + f64_ticks(arg_f64(a, 0), TICKS_PER_SEC), d.kind),
        "addmilliseconds" => dt(d.ticks + f64_ticks(arg_f64(a, 0), TICKS_PER_MS), d.kind),
        "addticks" => dt(d.ticks + a.int(0), d.kind),
        "addmonths" => dt(add_months(d.ticks, a.int(0)), d.kind),
        "addyears" => dt(add_months(d.ticks, a.int(0) * 12), d.kind),
        "add" => dt(d.ticks + arg_ts(a.raw(0)), d.kind),
        "subtract" => match arg_dt_strict(a.raw(0)) {
            Some(o) => timespan(d.ticks - o.ticks),
            None => dt(d.ticks - arg_ts(a.raw(0)), d.kind),
        },
        "compareto" => Val::Int(d.ticks.cmp(&arg_dt(a.raw(0)).map(|x| x.ticks).unwrap_or(0)) as i32),
        "tooadate" => {
            let base = naive_to_ticks(&NaiveDate::from_ymd_opt(1899, 12, 30).unwrap().and_hms_opt(0, 0, 0).unwrap());
            dec((d.ticks - base) as f64 / TICKS_PER_DAY as f64)
        }
        "tostring" => {
            let fmt = if a.is_empty() || matches!(a.raw(0), Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Culture(..))) { String::new() } else { a.text(0) };
            let pc = provider(a, if fmt.is_empty() { 0 } else { 1 });
            match try_format(d.ticks, Some(d.kind), None, &fmt, pc) {
                Some(s) => Val::Text(s),
                None => return net_err("DateTime", "ToString", "Input string was not in a correct format."),
            }
        }
        "toshortdatestring" => Val::Text(d.format("d", c)),
        "tolongdatestring" => Val::Text(d.format("D", c)),
        "toshorttimestring" => Val::Text(d.format("t", c)),
        "tolongtimestring" => Val::Text(d.format("T", c)),
        "tofiletimeutc" | "tofiletime" => Val::BigInt(d.ticks - 504_911_232_000_000_000),
        "tobinary" => Val::BigInt(d.ticks | ((d.kind as i64) << 62)),
        _ => return Ok(Out::Unknown),
    })
}

fn arg_dt_strict(v: Option<&Val>) -> Option<NetDateTime> {
    match v {
        Some(Val::DateTime(_)) => arg_dt(v),
        Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::DateTime(_) | NetObj::DateTimeOffset(..)) => arg_dt(v),
        _ => None,
    }
}

fn arg_f64(a: &Args, i: usize) -> f64 {
    a.get(i).as_dec().to_string().parse().unwrap_or(0.0)
}

fn dto_call(s: &mut Session, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
    match m {
        "now" => return ok(dto(now_ticks(), 0)),
        "utcnow" => return ok(dto(now_ticks(), 0)),
        "minvalue" => return ok(dto(0, 0)),
        "maxvalue" => return ok(dto(MAX_TICKS, 0)),
        "fromunixtimeseconds" => return ok(dto(EPOCH_TICKS + a.int(0) * TICKS_PER_SEC, 0)),
        "fromunixtimemilliseconds" => return ok(dto(EPOCH_TICKS + a.int(0) * TICKS_PER_MS, 0)),
        "fromfiletime" => return ok(dto(a.int(0) + 504_911_232_000_000_000, 0)),
        "parse" | "tryparse" | "parseexact" | "tryparseexact" => {
            let text = a.text(0);
            let exact = m.ends_with("exact");
            let c = provider(a, if exact { 2 } else { 1 });
            let p = if exact { parse_exact(text.trim(), &a.text(1), c) } else { parse_any(&text, c) };
            let assume_utc = (0..a.len()).any(|i| matches!(a.raw(i), Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Prim("System.Globalization.DateTimeStyles", v) if v.as_i64() & 64 != 0)));
            let v = p.map(|p| match p.offset {
                Some(off) => dto(p.utc_ticks(), off),
                None => {
                    let _ = assume_utc;
                    dto(p.ticks, 0)
                }
            });
            if m.starts_with("try") {
                let r = v.is_some();
                s.net_out(f, a, a.len() - 1, v.unwrap_or_else(|| dto(0, 0)))?;
                return ok(Val::Bool(r));
            }
            return match v {
                Some(v) => ok(v),
                None => net_err("DateTimeOffset", "Parse", format!("String '{text}' was not recognized as a valid DateTime.")),
            };
        }
        _ => {}
    }
    let (utc, off) = match &*n.borrow() {
        NetObj::DateTimeOffset(u, o) => (*u, *o),
        _ => return Ok(Out::Unknown),
    };
    let off_t = off as i64 * 60 * TICKS_PER_SEC;
    let clock = utc + off_t;
    let nd = ticks_to_naive(clock);
    ok(match m {
        "datetime" | "clockdatetime" => dt(clock, DtKind::Unspecified),
        "utcdatetime" => dt(utc, DtKind::Utc),
        "localdatetime" => dt(utc, DtKind::Local),
        "offset" => timespan(off_t),
        "ticks" => Val::BigInt(clock),
        "utcticks" => Val::BigInt(utc),
        "year" => Val::Int(nd.year()),
        "month" => Val::Int(nd.month() as i32),
        "day" => Val::Int(nd.day() as i32),
        "hour" => Val::Int(nd.hour() as i32),
        "minute" => Val::Int(nd.minute() as i32),
        "second" => Val::Int(nd.second() as i32),
        "millisecond" => Val::Int((clock.rem_euclid(TICKS_PER_SEC) / TICKS_PER_MS) as i32),
        "dayofweek" => enum_val("System.DayOfWeek", nd.weekday().num_days_from_sunday() as i64),
        "date" => dt(clock - clock.rem_euclid(TICKS_PER_DAY), DtKind::Unspecified),
        "timeofday" => timespan(clock.rem_euclid(TICKS_PER_DAY)),
        "tounixtimeseconds" => Val::BigInt((utc - EPOCH_TICKS).div_euclid(TICKS_PER_SEC)),
        "tounixtimemilliseconds" => Val::BigInt((utc - EPOCH_TICKS).div_euclid(TICKS_PER_MS)),
        "touniversaltime" => dto(utc, 0),
        "tolocaltime" => dto(utc, 0),
        "tooffset" => dto(utc, (arg_ts(a.raw(0)) / (60 * TICKS_PER_SEC)) as i32),
        "adddays" => dto(utc + f64_ticks(arg_f64(a, 0), TICKS_PER_DAY), off),
        "addhours" => dto(utc + f64_ticks(arg_f64(a, 0), 3600 * TICKS_PER_SEC), off),
        "addminutes" => dto(utc + f64_ticks(arg_f64(a, 0), 60 * TICKS_PER_SEC), off),
        "addseconds" => dto(utc + f64_ticks(arg_f64(a, 0), TICKS_PER_SEC), off),
        "addmilliseconds" => dto(utc + f64_ticks(arg_f64(a, 0), TICKS_PER_MS), off),
        "addticks" => dto(utc + a.int(0), off),
        "addmonths" => dto(add_months(clock, a.int(0)) - off_t, off),
        "addyears" => dto(add_months(clock, a.int(0) * 12) - off_t, off),
        "add" => dto(utc + arg_ts(a.raw(0)), off),
        "subtract" => match a.raw(0) {
            Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::DateTimeOffset(..)) => {
                let o = match &*x.borrow() {
                    NetObj::DateTimeOffset(u, _) => *u,
                    _ => 0,
                };
                timespan(utc - o)
            }
            v => dto(utc - arg_ts(v), off),
        },
        "compareto" => Val::Int(utc.cmp(&arg_dt(a.raw(0)).map(|x| x.ticks).unwrap_or(0)) as i32),
        "tostring" => {
            let fmt = if a.is_empty() || matches!(a.raw(0), Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Culture(..))) { String::new() } else { a.text(0) };
            let pc = provider(a, if fmt.is_empty() { 0 } else { 1 });
            match try_format(clock, None, Some(off), &fmt, pc) {
                Some(s) => Val::Text(s),
                None => return net_err("DateTimeOffset", "ToString", "Input string was not in a correct format."),
            }
        }
        _ => return Ok(Out::Unknown),
    })
}

pub fn format_offset(utc: i64, off: i32, fmt: &str, c: &Culture) -> String {
    format_ticks(utc + off as i64 * 60 * TICKS_PER_SEC, None, Some(off), fmt, c)
}

fn timespan_call(s: &mut Session, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let x = arg_f64(a, 0);
    match m {
        "fromticks" => return ok(timespan(a.int(0))),
        "frommilliseconds" => return ok(timespan(f64_ticks(x, TICKS_PER_MS))),
        "fromseconds" => return ok(timespan(f64_ticks(x, TICKS_PER_SEC))),
        "fromminutes" => return ok(timespan(f64_ticks(x, 60 * TICKS_PER_SEC))),
        "fromhours" => return ok(timespan(f64_ticks(x, 3600 * TICKS_PER_SEC))),
        "fromdays" => return ok(timespan(f64_ticks(x, TICKS_PER_DAY))),
        "zero" => return ok(timespan(0)),
        "maxvalue" => return ok(timespan(i64::MAX)),
        "minvalue" => return ok(timespan(i64::MIN)),
        "parse" | "tryparse" => {
            let v = parse_timespan(&a.text(0)).map(timespan);
            if m == "tryparse" {
                let r = v.is_some();
                s.net_out(f, a, a.len() - 1, v.unwrap_or_else(|| timespan(0)))?;
                return ok(Val::Bool(r));
            }
            return match v {
                Some(v) => ok(v),
                None => net_err("TimeSpan", "Parse", "String was not recognized as a valid TimeSpan."),
            };
        }
        _ => {}
    }
    let t = match &*n.borrow() {
        NetObj::TimeSpan(t) => *t,
        _ => return Ok(Out::Unknown),
    };
    ok(match m {
        "ticks" => Val::BigInt(t),
        "days" => Val::Int((t / TICKS_PER_DAY) as i32),
        "hours" => Val::Int(((t / (3600 * TICKS_PER_SEC)) % 24) as i32),
        "minutes" => Val::Int(((t / (60 * TICKS_PER_SEC)) % 60) as i32),
        "seconds" => Val::Int(((t / TICKS_PER_SEC) % 60) as i32),
        "milliseconds" => Val::Int(((t / TICKS_PER_MS) % 1000) as i32),
        "totaldays" => dec(t as f64 / TICKS_PER_DAY as f64),
        "totalhours" => dec(t as f64 / (3600 * TICKS_PER_SEC) as f64),
        "totalminutes" => dec(t as f64 / (60 * TICKS_PER_SEC) as f64),
        "totalseconds" => dec(t as f64 / TICKS_PER_SEC as f64),
        "totalmilliseconds" => dec(t as f64 / TICKS_PER_MS as f64),
        "add" => timespan(t + arg_ts(a.raw(0))),
        "subtract" => timespan(t - arg_ts(a.raw(0))),
        "negate" => timespan(-t),
        "duration" => timespan(t.abs()),
        "compareto" => Val::Int(t.cmp(&arg_ts(a.raw(0))) as i32),
        "tostring" => Val::Text(format_timespan(t, &a.text(0))),
        _ => return Ok(Out::Unknown),
    })
}

/// TimeSpan.ToString: "c" (default) `[-][d.]hh:mm:ss[.fffffff]`, "g", or a custom format.
pub fn format_timespan(t: i64, fmt: &str) -> String {
    let neg = t < 0;
    let a = t.unsigned_abs() as i64;
    let (d, h, mi, s, fr) = (a / TICKS_PER_DAY, (a / (3600 * TICKS_PER_SEC)) % 24, (a / (60 * TICKS_PER_SEC)) % 60, (a / TICKS_PER_SEC) % 60, a % TICKS_PER_SEC);
    match fmt {
        "" | "c" | "t" | "T" => {
            let mut out = String::new();
            if neg {
                out.push('-');
            }
            if d > 0 {
                out.push_str(&format!("{d}."));
            }
            out.push_str(&format!("{h:02}:{mi:02}:{s:02}"));
            if fr > 0 {
                out.push_str(&format!(".{fr:07}"));
            }
            out
        }
        "g" | "G" => {
            let mut out = String::new();
            if neg {
                out.push('-');
            }
            if d > 0 || fmt == "G" {
                out.push_str(&format!("{d}:"));
            }
            if fmt == "G" {
                out.push_str(&format!("{h:02}:{mi:02}:{s:02}.{fr:07}"));
            } else {
                out.push_str(&format!("{h}:{mi:02}:{s:02}"));
                if fr > 0 {
                    out.push_str(&format!(".{}", format!("{fr:07}").trim_end_matches('0')));
                }
            }
            out
        }
        _ => {
            let c: Vec<char> = fmt.chars().collect();
            let mut out = String::new();
            let mut i = 0;
            while i < c.len() {
                let ch = c[i];
                let mut k = 1;
                while i + k < c.len() && c[i + k] == ch {
                    k += 1;
                }
                match ch {
                    'd' => out.push_str(&format!("{:0w$}", d, w = k)),
                    'h' => out.push_str(&format!("{:0w$}", h, w = k.min(2))),
                    'm' => out.push_str(&format!("{:0w$}", mi, w = k.min(2))),
                    's' => out.push_str(&format!("{:0w$}", s, w = k.min(2))),
                    'f' => out.push_str(&format!("{fr:07}")[..k.min(7)]),
                    'F' => out.push_str(format!("{fr:07}")[..k.min(7)].trim_end_matches('0')),
                    '\\' => {
                        if let Some(x) = c.get(i + 1) {
                            out.push(*x);
                        }
                        i += 2;
                        continue;
                    }
                    '\'' | '"' => {
                        let mut j = i + 1;
                        while j < c.len() && c[j] != ch {
                            out.push(c[j]);
                            j += 1;
                        }
                        i = j + 1;
                        continue;
                    }
                    other => {
                        for _ in 0..k {
                            out.push(other);
                        }
                    }
                }
                i += k;
            }
            out
        }
    }
}

fn parse_timespan(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (days, rest) = match s.split_once('.') {
        Some((d, r)) if r.contains(':') && !d.contains(':') => (d.parse::<i64>().ok()?, r),
        _ => (0, s),
    };
    let (days, rest) = if !rest.contains(':') {
        (days + rest.parse::<i64>().ok()?, "0:0")
    } else {
        (days, rest)
    };
    let (hms, frac) = match rest.split_once('.') {
        Some((a, b)) => (a, b),
        None => (rest, ""),
    };
    let p: Vec<i64> = hms.split(':').map(|x| x.parse().unwrap_or(0)).collect();
    let (h, m, sec) = (p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0), p.get(2).copied().unwrap_or(0));
    let fr: i64 = if frac.is_empty() { 0 } else { format!("{:0<7}", &frac[..frac.len().min(7)]).parse().ok()? };
    let t = (((days * 24 + h) * 60 + m) * 60 + sec) * TICKS_PER_SEC + fr;
    Some(if neg { -t } else { t })
}

// ---------------------------------------------------------------- formatting

/// Format clock ticks. `kind` is set for DateTime, `offset` (minutes) for DateTimeOffset.
pub fn format_ticks(t: i64, kind: Option<DtKind>, offset: Option<i32>, fmt: &str, c: &Culture) -> String {
    try_format(t, kind, offset, fmt, c).unwrap_or_default()
}

pub fn try_format(t: i64, kind: Option<DtKind>, offset: Option<i32>, fmt: &str, c: &Culture) -> Option<String> {
    let pattern: String = if fmt.is_empty() {
        if offset.is_some() {
            format!("{} {} zzz", c.short_date, c.long_time)
        } else {
            format!("{} {}", c.short_date, c.long_time)
        }
    } else if fmt.chars().count() == 1 {
        let ch = fmt.chars().next().unwrap();
        match ch {
            'd' => c.short_date.into(),
            'D' => c.long_date.into(),
            't' => c.short_time.into(),
            'T' => c.long_time.into(),
            'f' => format!("{} {}", c.long_date, c.short_time),
            'F' => format!("{} {}", c.long_date, c.long_time),
            'g' => format!("{} {}", c.short_date, c.short_time),
            'G' => format!("{} {}", c.short_date, c.long_time),
            'M' | 'm' => c.month_day.into(),
            'Y' | 'y' => c.year_month.into(),
            's' => "yyyy'-'MM'-'dd'T'HH':'mm':'ss".into(),
            'O' | 'o' => {
                if offset.is_some() {
                    "yyyy'-'MM'-'dd'T'HH':'mm':'ss'.'fffffffzzz".into()
                } else {
                    "yyyy'-'MM'-'dd'T'HH':'mm':'ss'.'fffffffK".into()
                }
            }
            'R' | 'r' => {
                let utc = t - offset.unwrap_or(0) as i64 * 60 * TICKS_PER_SEC;
                return Some(format_custom(utc, kind, Some(0), "ddd, dd MMM yyyy HH':'mm':'ss 'GMT'", culture::invariant()));
            }
            'u' => {
                let utc = t - offset.unwrap_or(0) as i64 * 60 * TICKS_PER_SEC;
                return Some(format_custom(utc, kind, Some(0), "yyyy'-'MM'-'dd HH':'mm':'ss'Z'", culture::invariant()));
            }
            'U' => {
                let utc = t - offset.unwrap_or(0) as i64 * 60 * TICKS_PER_SEC;
                return Some(format_custom(utc, kind, Some(0), &format!("{} {}", c.long_date, c.long_time), c));
            }
            _ => return None,
        }
    } else {
        fmt.to_string()
    };
    let fixed = matches!(fmt, "s" | "O" | "o");
    Some(format_custom(t, kind, offset, &pattern, if fixed { culture::invariant() } else { c }))
}

pub fn format_custom(t: i64, kind: Option<DtKind>, offset: Option<i32>, pattern: &str, c: &Culture) -> String {
    let n = ticks_to_naive(t);
    let frac = t.rem_euclid(TICKS_PER_SEC);
    let c_: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let off_min = offset.unwrap_or(0);
    let off_text = |k: usize| -> String {
        let sign = if off_min < 0 { '-' } else { '+' };
        let a = off_min.abs();
        match k {
            1 => format!("{sign}{}", a / 60),
            2 => format!("{sign}{:02}", a / 60),
            _ => format!("{sign}{:02}:{:02}", a / 60, a % 60),
        }
    };
    while i < c_.len() {
        let ch = c_[i];
        let mut k = 1;
        while i + k < c_.len() && c_[i + k] == ch && !matches!(ch, '\'' | '"' | '\\' | '%') {
            k += 1;
        }
        match ch {
            'd' => match k {
                1 => out.push_str(&n.day().to_string()),
                2 => out.push_str(&format!("{:02}", n.day())),
                3 => out.push_str(&c.day_names[n.weekday().num_days_from_sunday() as usize][..3]),
                _ => out.push_str(c.day_names[n.weekday().num_days_from_sunday() as usize]),
            },
            'f' | 'F' => {
                let digits = format!("{frac:07}");
                let mut part = digits[..k.min(7)].to_string();
                if ch == 'F' {
                    part = part.trim_end_matches('0').to_string();
                    if part.is_empty() && out.ends_with('.') {
                        out.pop();
                    }
                }
                out.push_str(&part);
            }
            'g' => out.push_str("A.D."),
            'h' => {
                let h = match n.hour() % 12 {
                    0 => 12,
                    x => x,
                };
                out.push_str(&if k == 1 { h.to_string() } else { format!("{h:02}") });
            }
            'H' => out.push_str(&if k == 1 { n.hour().to_string() } else { format!("{:02}", n.hour()) }),
            'K' => {
                match (kind, offset) {
                    (Some(DtKind::Utc), _) => out.push('Z'),
                    (Some(DtKind::Local), _) => out.push_str("+00:00"),
                    (Some(DtKind::Unspecified), _) => {}
                    (None, Some(_)) => out.push_str(&off_text(3)),
                    _ => {}
                }
            }
            'm' => out.push_str(&if k == 1 { n.minute().to_string() } else { format!("{:02}", n.minute()) }),
            'M' => match k {
                1 => out.push_str(&n.month().to_string()),
                2 => out.push_str(&format!("{:02}", n.month())),
                3 => out.push_str(&c.month_names[n.month0() as usize][..3]),
                _ => out.push_str(c.month_names[n.month0() as usize]),
            },
            's' => out.push_str(&if k == 1 { n.second().to_string() } else { format!("{:02}", n.second()) }),
            't' => {
                let ampm = if n.hour() < 12 { c.am } else { c.pm };
                if k == 1 {
                    out.push_str(&ampm.chars().take(1).collect::<String>());
                } else {
                    out.push_str(ampm);
                }
            }
            'y' => {
                let y = n.year();
                match k {
                    1 => out.push_str(&(y % 100).to_string()),
                    2 => out.push_str(&format!("{:02}", y % 100)),
                    _ => out.push_str(&format!("{:0w$}", y, w = k)),
                }
            }
            'z' => out.push_str(&off_text(k)),
            ':' => out.push_str(c.time_sep),
            '/' => out.push_str(c.date_sep),
            '\'' | '"' => {
                let mut j = i + 1;
                while j < c_.len() && c_[j] != ch {
                    if c_[j] == '\\' && j + 1 < c_.len() {
                        j += 1;
                    }
                    out.push(c_[j]);
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            '\\' => {
                if let Some(x) = c_.get(i + 1) {
                    out.push(*x);
                }
                i += 2;
                continue;
            }
            '%' => {
                i += 1;
                continue;
            }
            other => {
                for _ in 0..k {
                    out.push(other);
                }
            }
        }
        i += k;
    }
    out
}

// ---------------------------------------------------------------- parsing

pub struct Parsed {
    /// Clock ticks as written.
    pub ticks: i64,
    /// Offset in minutes when the text had one (Z = 0).
    pub offset: Option<i32>,
    pub zulu: bool,
}

impl Parsed {
    pub fn utc_ticks(&self) -> i64 {
        self.ticks - self.offset.unwrap_or(0) as i64 * 60 * TICKS_PER_SEC
    }
    pub fn datetime(&self) -> NetDateTime {
        match self.offset {
            Some(_) => NetDateTime { ticks: self.utc_ticks(), kind: DtKind::Local },
            None => NetDateTime { ticks: self.ticks, kind: DtKind::Unspecified },
        }
    }
}

/// DateTime.Parse: ISO 8601, RFC 1123 and the culture's date/time patterns.
pub fn parse_any(s: &str, c: &Culture) -> Option<Parsed> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // ISO 8601 / round-trip
    if let Some(p) = parse_iso(s) {
        return Some(p);
    }
    // RFC 1123
    if let Ok(d) = chrono::DateTime::parse_from_rfc2822(s) {
        return Some(Parsed { ticks: naive_to_ticks(&d.naive_local()), offset: Some(d.offset().local_minus_utc() / 60), zulu: false });
    }
    // culture patterns: date [time]
    let (dpart, tpart) = match s.find(' ') {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };
    let date = parse_culture_date(dpart, c)?;
    let time = if tpart.is_empty() { 0 } else { parse_time_of_day(tpart)? };
    Some(Parsed { ticks: naive_to_ticks(&date.and_hms_opt(0, 0, 0)?) + time, offset: None, zulu: false })
}

fn parse_culture_date(s: &str, c: &Culture) -> Option<NaiveDate> {
    let seps: &[char] = &['/', '-', '.'];
    let parts: Vec<&str> = s.split(seps).collect();
    if parts.len() != 3 {
        return None;
    }
    let nums: Vec<i32> = parts.iter().map(|p| p.parse::<i32>()).collect::<Result<_, _>>().ok()?;
    let order = c.date_order();
    let (y, m, d) = if parts[0].len() == 4 {
        (nums[0], nums[1], nums[2])
    } else {
        match order {
            "mdy" => (nums[2], nums[0], nums[1]),
            "dmy" => (nums[2], nums[1], nums[0]),
            _ => (nums[0], nums[1], nums[2]),
        }
    };
    let y = if y < 100 { if y < 50 { 2000 + y } else { 1900 + y } } else { y };
    NaiveDate::from_ymd_opt(y, m as u32, d as u32)
}

fn parse_time_of_day(s: &str) -> Option<i64> {
    let up = s.to_ascii_uppercase();
    let pm = up.ends_with("PM");
    let am = up.ends_with("AM");
    let core = up.trim_end_matches("AM").trim_end_matches("PM").trim();
    let (hms, frac) = match core.split_once('.') {
        Some((a, b)) => (a, b),
        None => (core, ""),
    };
    let p: Vec<&str> = hms.split(':').collect();
    let mut h: i64 = p.first()?.parse().ok()?;
    let m: i64 = p.get(1).map(|x| x.parse().ok()).unwrap_or(Some(0))?;
    let sec: i64 = p.get(2).map(|x| x.parse().ok()).unwrap_or(Some(0))?;
    if pm && h < 12 {
        h += 12;
    }
    if am && h == 12 {
        h = 0;
    }
    let fr: i64 = if frac.is_empty() { 0 } else { format!("{:0<7}", &frac[..frac.len().min(7)]).parse().ok()? };
    Some(((h * 60 + m) * 60 + sec) * TICKS_PER_SEC + fr)
}

/// `yyyy-MM-dd[THH:mm[:ss[.fffffff]]][Z|±hh:mm]`.
pub fn parse_iso(s: &str) -> Option<Parsed> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let mo: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    let date = NaiveDate::from_ymd_opt(y, mo, d)?;
    let mut rest = &s[10..];
    let mut time = 0i64;
    if rest.starts_with('T') || rest.starts_with(' ') {
        rest = &rest[1..];
        let end = rest.find(|c: char| c == 'Z' || c == '+' || c == '-').unwrap_or(rest.len());
        let tp = &rest[..end];
        time = parse_time_of_day(tp)?;
        rest = &rest[end..];
    }
    let (offset, zulu) = if rest.is_empty() {
        (None, false)
    } else if rest == "Z" || rest == "z" {
        (Some(0), true)
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let r = &rest[1..];
        let (hh, mm) = match r.split_once(':') {
            Some((a, b)) => (a.parse::<i32>().ok()?, b.parse::<i32>().ok()?),
            None if r.len() == 4 => (r[..2].parse().ok()?, r[2..].parse().ok()?),
            None => (r.parse().ok()?, 0),
        };
        (Some(sign * (hh * 60 + mm)), false)
    };
    Some(Parsed { ticks: naive_to_ticks(&date.and_hms_opt(0, 0, 0)?) + time, offset, zulu })
}

/// ParseExact with a custom (or standard one-letter) format.
pub fn parse_exact(s: &str, fmt: &str, c: &Culture) -> Option<Parsed> {
    let pattern: String = match fmt {
        "o" | "O" => return parse_iso(s),
        "s" => "yyyy-MM-ddTHH:mm:ss".into(),
        "u" => "yyyy-MM-dd HH:mm:ssZ".into(),
        "r" | "R" => return chrono::DateTime::parse_from_rfc2822(s).ok().map(|d| Parsed { ticks: naive_to_ticks(&d.naive_utc()), offset: Some(0), zulu: true }),
        "d" => c.short_date.into(),
        "D" => c.long_date.into(),
        "t" => c.short_time.into(),
        "T" => c.long_time.into(),
        "g" => format!("{} {}", c.short_date, c.short_time),
        "G" => format!("{} {}", c.short_date, c.long_time),
        other => other.to_string(),
    };
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    let (mut y, mut mo, mut d, mut h, mut mi, mut sec, mut fr) = (1i32, 1u32, 1u32, 0i64, 0i64, 0i64, 0i64);
    let mut pm: Option<bool> = None;
    let mut offset: Option<i32> = None;
    let mut zulu = false;
    let num = |j: &mut usize, min: usize, max: usize| -> Option<i64> {
        let st = *j;
        while *j < t.len() && *j - st < max && t[*j].is_ascii_digit() {
            *j += 1;
        }
        if *j - st < min {
            return None;
        }
        t[st..*j].iter().collect::<String>().parse().ok()
    };
    while i < p.len() {
        let ch = p[i];
        let mut k = 1;
        while i + k < p.len() && p[i + k] == ch && !matches!(ch, '\'' | '"' | '\\') {
            k += 1;
        }
        match ch {
            'y' => {
                let v = num(&mut j, k.min(4), if k <= 2 { 2 } else { k.max(4) })? as i32;
                y = if k <= 2 { 2000 + v } else { v };
            }
            'M' if k >= 3 => {
                let names = &c.month_names;
                let rest: String = t[j..].iter().collect();
                let idx = names.iter().position(|n| if k == 3 { rest.to_lowercase().starts_with(&n[..3].to_lowercase()) } else { rest.to_lowercase().starts_with(&n.to_lowercase()) })?;
                mo = idx as u32 + 1;
                j += if k == 3 { 3 } else { names[idx].chars().count() };
            }
            'M' => mo = num(&mut j, k, 2)? as u32,
            'd' if k >= 3 => {
                while j < t.len() && t[j].is_alphabetic() {
                    j += 1;
                }
            }
            'd' => d = num(&mut j, k, 2)? as u32,
            'H' | 'h' => h = num(&mut j, k, 2)?,
            'm' => mi = num(&mut j, k, 2)?,
            's' => sec = num(&mut j, k, 2)?,
            'f' | 'F' => {
                let st = j;
                let v = num(&mut j, if ch == 'f' { k } else { 0 }, k).unwrap_or(0);
                let digits = j - st;
                fr = v * 10i64.pow((7 - digits.min(7)) as u32);
            }
            't' => {
                let rest: String = t[j..].iter().collect::<String>().to_ascii_uppercase();
                if rest.starts_with("PM") || rest.starts_with('P') {
                    pm = Some(true);
                } else {
                    pm = Some(false);
                }
                j += k.min(2).min(t.len() - j);
            }
            'z' | 'K' => {
                if t.get(j) == Some(&'Z') {
                    offset = Some(0);
                    zulu = true;
                    j += 1;
                } else if matches!(t.get(j), Some('+') | Some('-')) {
                    let sign = if t[j] == '-' { -1 } else { 1 };
                    j += 1;
                    let hh = num(&mut j, 1, 2)? as i32;
                    let mut mm = 0;
                    if t.get(j) == Some(&':') {
                        j += 1;
                        mm = num(&mut j, 2, 2)? as i32;
                    }
                    offset = Some(sign * (hh * 60 + mm));
                }
            }
            '\'' | '"' => {
                let mut q = i + 1;
                while q < p.len() && p[q] != ch {
                    if t.get(j) != Some(&p[q]) {
                        return None;
                    }
                    j += 1;
                    q += 1;
                }
                i = q + 1;
                continue;
            }
            '\\' => {
                if t.get(j) != p.get(i + 1) {
                    return None;
                }
                j += 1;
                i += 2;
                continue;
            }
            other => {
                for _ in 0..k {
                    let want = match other {
                        ':' => c.time_sep.chars().next().unwrap_or(':'),
                        '/' => c.date_sep.chars().next().unwrap_or('/'),
                        x => x,
                    };
                    if t.get(j) != Some(&want) && !(other == 'Z' && t.get(j) == Some(&'Z')) {
                        return None;
                    }
                    if other == 'Z' {
                        offset = Some(0);
                        zulu = true;
                    }
                    j += 1;
                }
            }
        }
        i += k;
    }
    if j != t.len() {
        return None;
    }
    if let Some(p) = pm {
        if p && h < 12 {
            h += 12;
        }
        if !p && h == 12 {
            h = 0;
        }
    }
    let date = NaiveDate::from_ymd_opt(y, mo, d)?;
    Some(Parsed { ticks: naive_to_ticks(&date.and_hms_opt(0, 0, 0)?) + ((h * 60 + mi) * 60 + sec) * TICKS_PER_SEC + fr, offset, zulu })
}

// ---------------------------------------------------------------- time zones

pub struct Zone {
    pub id: String,
    pub tz: Option<chrono_tz::Tz>,
    /// Fixed offset in minutes (custom zones, UTC).
    pub fixed: Option<i32>,
    pub display: Option<String>,
}

impl Zone {
    pub fn utc() -> Zone {
        Zone { id: "UTC".into(), tz: None, fixed: Some(0), display: Some("(UTC) Coordinated Universal Time".into()) }
    }

    /// Offset (minutes) at a UTC instant.
    pub fn offset_at_utc(&self, utc_ticks: i64) -> i32 {
        use chrono::Offset;
        match (self.fixed, &self.tz) {
            (Some(f), _) => f,
            (None, Some(tz)) => {
                let n = ticks_to_naive(utc_ticks);
                let d = chrono::TimeZone::from_utc_datetime(tz, &n);
                d.offset().fix().local_minus_utc() / 60
            }
            _ => 0,
        }
    }

    /// Offset (minutes) for a clock time in this zone.
    pub fn offset_at_local(&self, clock_ticks: i64) -> i32 {
        use chrono::Offset;
        match (self.fixed, &self.tz) {
            (Some(f), _) => f,
            (None, Some(tz)) => {
                let n = ticks_to_naive(clock_ticks);
                match chrono::TimeZone::offset_from_local_datetime(tz, &n) {
                    chrono::LocalResult::Single(o) => o.fix().local_minus_utc() / 60,
                    chrono::LocalResult::Ambiguous(a, _) => a.fix().local_minus_utc() / 60,
                    // in a spring-forward gap: the standard offset
                    chrono::LocalResult::None => self.base_offset(),
                }
            }
            _ => 0,
        }
    }

    pub fn base_offset(&self) -> i32 {
        use chrono_tz::OffsetComponents;
        match (self.fixed, &self.tz) {
            (Some(f), _) => f,
            (None, Some(tz)) => {
                let n = chrono::Utc::now().naive_utc();
                let o = chrono::TimeZone::offset_from_utc_datetime(tz, &n);
                (o.base_utc_offset().num_minutes()) as i32
            }
            _ => 0,
        }
    }

    pub fn is_dst_utc(&self, utc_ticks: i64) -> bool {
        use chrono_tz::OffsetComponents;
        match &self.tz {
            Some(tz) if self.fixed.is_none() => {
                let n = ticks_to_naive(utc_ticks);
                let o = chrono::TimeZone::offset_from_utc_datetime(tz, &n);
                o.dst_offset().num_minutes() != 0
            }
            _ => false,
        }
    }

    pub fn supports_dst(&self) -> bool {
        let Some(_) = &self.tz else { return false };
        if self.fixed.is_some() {
            return false;
        }
        // like Windows adjustment rules: any DST period since 1970 counts
        let now = chrono::Utc::now().year();
        (1970..=now + 1).any(|y| {
            [1u32, 4, 7, 10].iter().any(|m| self.is_dst_utc(naive_to_ticks(&NaiveDate::from_ymd_opt(y, *m, 15).unwrap().and_hms_opt(12, 0, 0).unwrap())))
        })
    }

    pub fn display(&self) -> String {
        if let Some(d) = &self.display {
            return d.clone();
        }
        let b = self.base_offset();
        let sign = if b < 0 { '-' } else { '+' };
        format!("(UTC{sign}{:02}:{:02}) {}", b.abs() / 60, b.abs() % 60, self.id)
    }
}

/// Windows time zone ids (as stored in BC) → IANA ids (CLDR windowsZones, territory 001).
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("UTC-11", "Etc/GMT+11"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("UTC-09", "Etc/GMT+9"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("UTC-08", "Etc/GMT+8"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Mountain Standard Time", "America/Denver"),
    ("Yukon Standard Time", "America/Whitehorse"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time", "America/Chicago"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Canada Central Standard Time", "America/Regina"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Eastern Standard Time", "America/New_York"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Cuba Standard Time", "America/Havana"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Bahia Standard Time", "America/Bahia"),
    ("UTC-02", "Etc/GMT+2"),
    ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("UTC", "Etc/UTC"),
    ("Coordinated Universal Time", "Etc/UTC"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Sudan Standard Time", "Africa/Juba"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Volgograd Standard Time", "Europe/Volgograd"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Qyzylorda Standard Time", "Asia/Qyzylorda"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("Central Asia Standard Time", "Asia/Bishkek"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("China Standard Time", "Asia/Shanghai"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC+12", "Etc/GMT-12"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("Kamchatka Standard Time", "Asia/Kamchatka"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("UTC+13", "Etc/GMT-13"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
];

pub fn find_zone(id: &str) -> Option<Zone> {
    let id_t = id.trim();
    if id_t.eq_ignore_ascii_case("utc") || id_t.eq_ignore_ascii_case("etc/utc") || id_t.eq_ignore_ascii_case("coordinated universal time") {
        return Some(Zone { id: id_t.to_string(), ..Zone::utc() });
    }
    let iana = WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(id_t)).map(|(_, i)| *i).unwrap_or(id_t);
    let tz: chrono_tz::Tz = iana.parse().ok()?;
    Some(Zone { id: id_t.to_string(), tz: Some(tz), fixed: None, display: None })
}

pub fn windows_zone_ids() -> impl Iterator<Item = &'static str> {
    WINDOWS_ZONES.iter().map(|(w, _)| *w)
}

fn zone_arg(a: &Args, i: usize) -> Option<Rc<Zone>> {
    match a.raw(i) {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::TimeZone(z) => Some(z.clone()),
            NetObj::Prim(_, Val::Text(s)) => find_zone(s).map(Rc::new),
            _ => None,
        },
        Some(Val::Text(s)) => find_zone(s).map(Rc::new),
        _ => None,
    }
}

fn tz_call(n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let zone_val = |z: Zone| net(NetObj::TimeZone(Rc::new(z)));
    match m {
        "findsystemtimezonebyid" => {
            let id = a.text(0);
            return match find_zone(&id) {
                Some(z) => ok(zone_val(z)),
                None => net_err("TimeZoneInfo", "FindSystemTimeZoneById", format!("The time zone ID '{id}' was not found on the local computer.")),
            };
        }
        "utc" | "local" => return ok(zone_val(Zone::utc())),
        "getsystemtimezones" => {
            let v: Vec<Val> = windows_zone_ids().filter_map(find_zone).map(zone_val).collect();
            return ok(net(NetObj::List("System.Collections.ObjectModel.ReadOnlyCollection`1", v)));
        }
        "createcustomtimezone" => {
            let off = (arg_ts(a.raw(1)) / (60 * TICKS_PER_SEC)) as i32;
            return ok(zone_val(Zone { id: a.text(0), tz: None, fixed: Some(off), display: Some(a.text(2)) }));
        }
        "converttimefromutc" | "converttime" | "converttimebysystemtimezoneid" | "converttimetoutc" => {
            let d = arg_dt(a.raw(0)).unwrap_or(NetDateTime { ticks: 0, kind: DtKind::Utc });
            let is_dto = matches!(a.raw(0), Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::DateTimeOffset(..)));
            let (src, dst): (Option<Rc<Zone>>, Option<Rc<Zone>>) = match (m, a.len()) {
                ("converttimefromutc", _) => (Some(Rc::new(Zone::utc())), zone_arg(a, 1)),
                ("converttimetoutc", 1) => (Some(Rc::new(Zone::utc())), Some(Rc::new(Zone::utc()))),
                ("converttimetoutc", _) => (zone_arg(a, 1), Some(Rc::new(Zone::utc()))),
                (_, 2) => (None, zone_arg(a, 1)),
                (_, _) => (zone_arg(a, 1), zone_arg(a, 2)),
            };
            let Some(dst) = dst else { return net_err("TimeZoneInfo", "ConvertTime", "The time zone was not found.") };
            // the instant: from the source zone's clock time, or the DateTime's kind
            let utc = if is_dto {
                match a.raw(0) {
                    Some(Val::Net(x)) => match &*x.borrow() {
                        NetObj::DateTimeOffset(u, _) => *u,
                        _ => 0,
                    },
                    _ => 0,
                }
            } else {
                match (&src, d.kind) {
                    (Some(z), DtKind::Unspecified) => d.ticks - z.offset_at_local(d.ticks) as i64 * 60 * TICKS_PER_SEC,
                    _ => d.ticks,
                }
            };
            let off = dst.offset_at_utc(utc);
            if is_dto {
                return ok(dto(utc, off));
            }
            let kind = if dst.fixed == Some(0) && dst.tz.is_none() { DtKind::Utc } else { DtKind::Unspecified };
            return ok(dt(utc + off as i64 * 60 * TICKS_PER_SEC, kind));
        }
        _ => {}
    }
    let z = match &*n.borrow() {
        NetObj::TimeZone(z) => z.clone(),
        _ => return Ok(Out::Unknown),
    };
    // the instant of a DateTime argument: kind Utc/Local is an instant, Unspecified is a clock time in this zone
    let instant = |v: Option<&Val>| -> i64 {
        match v {
            Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::DateTimeOffset(..)) => match &*x.borrow() {
                NetObj::DateTimeOffset(u, _) => *u,
                _ => 0,
            },
            v => {
                let d = arg_dt(v).unwrap_or(NetDateTime { ticks: 0, kind: DtKind::Utc });
                match d.kind {
                    DtKind::Unspecified => d.ticks - z.offset_at_local(d.ticks) as i64 * 60 * TICKS_PER_SEC,
                    _ => d.ticks,
                }
            }
        }
    };
    ok(match m {
        "id" => Val::Text(z.id.clone()),
        "displayname" => Val::Text(z.display()),
        "standardname" | "daylightname" => Val::Text(z.id.clone()),
        "baseutcoffset" => timespan(z.base_offset() as i64 * 60 * TICKS_PER_SEC),
        "getutcoffset" => timespan(z.offset_at_utc(instant(a.raw(0))) as i64 * 60 * TICKS_PER_SEC),
        "supportsdaylightsavingtime" => Val::Bool(z.supports_dst()),
        "isdaylightsavingtime" => Val::Bool(z.is_dst_utc(instant(a.raw(0)))),
        "tostring" => Val::Text(z.display()),
        _ => return Ok(Out::Unknown),
    })
}

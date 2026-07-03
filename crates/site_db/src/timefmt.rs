//! Deterministic time formatting from an injected build epoch (SOURCE_DATE_EPOCH
//! style). NEVER wall clock — a wall-clock genDate makes every build dirty by
//! construction (§2c determinism prerequisite). All formats are UTC so a given
//! epoch always yields identical bytes regardless of the builder's timezone.
//!
//! The three consumed formats mirror rows.ts / resource-metadata.ts. The row
//! comparator (and cycle's compare.ts) ignore genDate/genDay for parity, so UTC
//! rather than the TS local-time output is intentional and safe.

fn days_from_civil(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

struct Utc {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    weekday: u32, // 0=Sun
}

fn to_utc(epoch_secs: i64) -> Utc {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = days_from_civil(days);
    // 1970-01-01 was a Thursday (weekday 4).
    let weekday = (((days % 7) + 4) % 7 + 7) as u32 % 7;
    Utc {
        year,
        month,
        day,
        hour: (secs_of_day / 3600) as u32,
        min: ((secs_of_day % 3600) / 60) as u32,
        sec: (secs_of_day % 60) as u32,
        weekday,
    }
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// rows.ts:119 formatGenDate — "Wed, Jul 03, 2026 14:05+0000" (UTC).
pub fn gen_date(epoch_secs: i64) -> String {
    let t = to_utc(epoch_secs);
    format!(
        "{}, {} {:02}, {} {:02}:{:02}+0000",
        WEEKDAYS[t.weekday as usize],
        MONTHS[(t.month - 1) as usize],
        t.day,
        t.year,
        t.hour,
        t.min,
    )
}

/// rows.ts:295 genDay — "DD/MM/YYYY".
pub fn gen_day(epoch_secs: i64) -> String {
    let t = to_utc(epoch_secs);
    format!("{:02}/{:02}/{}", t.day, t.month, t.year)
}

/// resource-metadata.ts:46 formatFhirDateTime — "YYYY-MM-DDThh:mm:ss+00:00" (UTC).
pub fn fhir_datetime(epoch_secs: i64) -> String {
    let t = to_utc(epoch_secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        t.year, t.month, t.day, t.hour, t.min, t.sec
    )
}

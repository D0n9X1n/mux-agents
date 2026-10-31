//! Hashes, random ids and the `createdAt` time text.

use std::io;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

const TICKS_PER_SEC: i64 = 10_000_000;

/// SHA-256 of the bytes (UTF-8 for text) as lowercase hex.
pub fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    hex(&Sha256::digest(data.as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

#[cfg(windows)]
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    for chunk in buf.chunks_mut(u32::MAX as usize) {
        // SAFETY: the pointer and length describe one live, writable chunk of `buf`.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(io::Error::other("BCryptGenRandom failed"));
        }
    }
    Ok(())
}

/// 12 lowercase hex characters: an agent id, like the old `NewGuid().ToString('N').Substring(0, 12)`.
pub fn hex_id() -> io::Result<String> {
    let mut bytes = [0u8; 6];
    random_bytes(&mut bytes)?;
    Ok(hex(&bytes))
}

/// A random (version 4) GUID in lowercase 8-4-4-4-12 form: a client session id.
pub fn guid() -> io::Result<String> {
    let mut b = [0u8; 16];
    random_bytes(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's days_from_civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    days_from_civil(year + i64::from(month == 12), month % 12 + 1, 1)
        - days_from_civil(year, month, 1)
}

/// .NET's round-trip `ToString('o')` of a UTC time: `YYYY-MM-DDTHH:MM:SS.fffffffZ`, in 100 ns ticks.
pub fn utc_o(time: SystemTime) -> String {
    let ticks = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => (after.as_nanos() / 100) as i64,
        Err(before) => -(before.duration().as_nanos().div_ceil(100) as i64),
    };
    let secs = ticks.div_euclid(TICKS_PER_SEC);
    let fraction = ticks.rem_euclid(TICKS_PER_SEC);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction:07}Z",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

fn digits(text: &[u8], width: usize) -> Option<i64> {
    let part = text.get(..width)?;
    part.iter()
        .all(u8::is_ascii_digit)
        .then(|| part.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
}

/// A state-file time written as `YYYY-MM-DDTHH:MM:SS[.f{1,7}]` then `Z` or `±HH:MM`; `None` when unreadable.
/// Our writers always add the zone, so a zoneless time is unreadable rather than read as local.
pub fn parse_utc(text: &str) -> Option<SystemTime> {
    let b = text.as_bytes();
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if b.len() < 20 || separators.iter().any(|&(i, c)| b[i] != c) {
        return None;
    }
    let (year, month, day) = (digits(b, 4)?, digits(&b[5..], 2)?, digits(&b[8..], 2)?);
    let (hour, minute, second) = (
        digits(&b[11..], 2)?,
        digits(&b[14..], 2)?,
        digits(&b[17..], 2)?,
    );
    if year < 1 || !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut rest = &b[19..];
    let mut nanos = 0i64;
    if let Some(after) = rest.strip_prefix(b".") {
        let width = after.iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=7).contains(&width) {
            return None;
        }
        nanos = digits(after, width)? * 10i64.pow(9 - width as u32);
        rest = &after[width..];
    }
    let offset = match rest {
        b"Z" => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let (hours, minutes) = (digits(&rest[1..], 2)?, digits(&rest[4..], 2)?);
            if minutes > 59 || hours * 60 + minutes > 14 * 60 {
                return None;
            }
            let span = (hours * 60 + minutes) * 60;
            if *sign == b'+' { span } else { -span }
        }
        _ => return None,
    };
    let secs =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    let whole = Duration::from_secs(secs.unsigned_abs());
    let base = if secs >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(whole)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(whole)
    };
    base?.checked_add(Duration::from_nanos(nanos as u64))
}

/// Whole seconds from `created_at` to `now`, at least 0; `None` when `created_at` is unreadable.
pub fn age_seconds(created_at: &str, now: SystemTime) -> Option<u64> {
    let created = parse_utc(created_at)?;
    Some(now.duration_since(created).map_or(0, |age| age.as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn at(secs: u64, nanos: u32) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::new(secs, nanos)
    }

    #[test]
    fn sha256_hex_known_vectors() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hex_id_is_12_lower_hex() {
        let id = hex_id().unwrap();
        assert_eq!(id.len(), 12);
        assert!(
            id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{id}"
        );
        assert_ne!(id, hex_id().unwrap());
    }

    #[test]
    fn guid_is_v4_lower() {
        let guid = guid().unwrap();
        let groups: Vec<_> = guid.split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12], "{guid}");
        assert!(
            guid.bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-')),
            "{guid}"
        );
        assert_eq!(&guid[14..15], "4");
        assert!("89ab".contains(&guid[19..20]), "{guid}");
        assert_ne!(guid, super::guid().unwrap());
    }

    #[test]
    fn utc_o_has_7_fraction_digits_and_z() {
        assert_eq!(utc_o(at(0, 0)), "1970-01-01T00:00:00.0000000Z");
        // Ticks are 100 ns; finer digits are dropped, not rounded.
        assert_eq!(
            utc_o(at(1_700_000_000, 123_456_789)),
            "2023-11-14T22:13:20.1234567Z"
        );
        assert_eq!(utc_o(at(951_782_400, 0)), "2000-02-29T00:00:00.0000000Z");
        assert_eq!(
            utc_o(at(4_107_542_399, 999_999_999)),
            "2100-02-28T23:59:59.9999999Z"
        );
    }

    #[test]
    fn parse_utc_accepts_offset_and_z() {
        let t = at(1_700_000_000, 123_456_700);
        assert_eq!(parse_utc("2023-11-14T22:13:20.1234567Z"), Some(t));
        assert_eq!(parse_utc("2023-11-15T00:13:20.1234567+02:00"), Some(t));
        assert_eq!(parse_utc("2023-11-14T17:13:20.1234567-05:00"), Some(t));
        assert_eq!(
            parse_utc("2023-11-14T22:13:20Z"),
            Some(at(1_700_000_000, 0))
        );
        assert_eq!(
            parse_utc("2023-11-14T22:13:20.5Z"),
            Some(at(1_700_000_000, 500_000_000))
        );
        assert_eq!(parse_utc(&utc_o(t)), Some(t));
        for bad in [
            "",
            "not a time",
            "2023-11-14T22:13:20",
            "2023-02-29T00:00:00Z",
            "2023-13-01T00:00:00Z",
            "2023-11-14T24:00:00Z",
            "2023-11-14T22:13:20.Z",
            "2023-11-14T22:13:20+15:00",
            "2023-11-14 22:13:20Z",
            "2023-11-14T22:13:20.12345678Z",
            "2023-11-14T22:13:20Zjunk",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad}");
        }
    }

    #[test]
    fn age_floor_seconds_min_0() {
        let created = "2023-11-14T22:13:20.0000000Z";
        assert_eq!(
            age_seconds(created, at(1_700_000_005, 900_000_000)),
            Some(5)
        );
        assert_eq!(age_seconds(created, at(1_700_000_000, 0)), Some(0));
        assert_eq!(age_seconds(created, at(1_699_999_000, 0)), Some(0));
        assert_eq!(age_seconds("garbage", at(1_700_000_005, 0)), None);
    }
}

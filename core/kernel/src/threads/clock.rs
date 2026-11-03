//! ISO 8601 dates as the Python side and the host read them, without a date crate.

/// What a date text said: the clock reading as if it were UTC, and the offset it named, if any.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    /// Seconds since 1970-01-01 of the clock reading taken as UTC.
    pub clock: f64,
    /// Seconds east of UTC the text named (`Z` is 0); `None` for a text without one.
    pub offset: Option<i64>,
    /// The text had a time of day (a date alone reads as midnight).
    pub timed: bool,
}

impl Stamp {
    /// The instant it names: the clock minus the offset; a text without one is read as UTC.
    pub fn epoch(self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let off = self.offset.unwrap_or(0) as f64;
        self.clock - off
    }
}

/// Days since 1970-01-01 of a civil date (H. Hinnant's algorithm); `None` for a date that does not exist.
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || d < 1 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if d > dim[usize::try_from(m - 1).ok()?] {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn digits(s: &str, n: usize) -> Option<(i64, &str)> {
    let (head, rest) = (s.get(..n)?, s.get(n..)?);
    head.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| head.parse().ok().map(|v| (v, rest)))?
}

/// `YYYY-MM-DD[(T| )HH:MM[:SS[.fff]]][Z|(+|-)HH[:MM]]`, as Python's `datetime.fromisoformat` reads it (3.11 and later,
/// the shapes that appear in Claude's and the kernel's files). `None` for anything else.
#[allow(clippy::many_single_char_names)]
pub fn parse(text: &str) -> Option<Stamp> {
    let (y, rest) = digits(text, 4)?;
    let rest = rest.strip_prefix('-')?;
    let (mo, rest) = digits(rest, 2)?;
    let rest = rest.strip_prefix('-')?;
    let (d, rest) = digits(rest, 2)?;
    let days = days_from_civil(y, mo, d)?;
    #[allow(clippy::cast_precision_loss)]
    let mut clock = days as f64 * 86_400.0;
    if rest.is_empty() {
        return Some(Stamp {
            clock,
            offset: None,
            timed: false,
        });
    }
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let (h, rest) = digits(rest, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (mi, mut rest) = digits(rest, 2)?;
    let mut sec = 0;
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix(':') {
        let (s, r) = digits(r, 2)?;
        sec = s;
        rest = r;
        if let Some(r) = rest.strip_prefix(['.', ',']) {
            let n = r.bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                return None;
            }
            frac = format!("0.{}", &r[..n]).parse().ok()?;
            rest = &r[n..];
        }
    }
    if h > 24 || mi > 59 || sec > 59 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        clock += (h * 3600 + mi * 60 + sec) as f64 + frac;
    }
    let offset = match rest {
        "" => None,
        "Z" | "z" => Some(0),
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (oh, r) = digits(&rest[1..], 2)?;
            let (om, r) = match r.strip_prefix(':') {
                Some(r) => digits(r, 2)?,
                None if r.is_empty() => (0, r),
                None => digits(r, 2)?,
            };
            if !r.is_empty() {
                return None;
            }
            Some(sign * (oh * 3600 + om * 60))
        }
    };
    Some(Stamp {
        clock,
        offset,
        timed: true,
    })
}

/// RFC 3339 in milliseconds (`chrono::DateTime::parse_from_rfc3339`): date, time with seconds and an offset are all required.
pub fn rfc3339_ms(text: &str) -> Option<i64> {
    let s = parse(text)?;
    if !s.timed || s.offset.is_none() || text.len() < 20 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some((s.epoch() * 1000.0).round() as i64)
}

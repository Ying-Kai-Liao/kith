//! Python's number text, as `kernel/bin/kith-cost` prints it: `f'{x:,.2f}'`, `round(x, n)`, `str(float)`, `shlex.quote` and
//! float floor division. Rust's `{:.N}` is exact too, but a tie would round the way the platform's libm does; these round
//! the exact decimal value half to even, as `CPython` does.

/// The exact decimal text of a finite `x` to `prec` places, rounded half to even on the exact value (`'%.*f' % (prec, x)`).
pub fn fixed(x: f64, prec: usize) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".to_owned()
        } else if x > 0.0 {
            "inf".to_owned()
        } else {
            "-inf".to_owned()
        };
    }
    // 1100 places hold every f64 exactly (the smallest subnormal has 1074), so the text below is the exact value.
    let exact = format!("{:.1100}", x.abs());
    let (int_part, frac) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits: Vec<u8> = int_part.bytes().chain(frac.bytes().take(prec)).collect();
    let rest = &frac.as_bytes()[prec.min(frac.len())..];
    let round_up = match rest.first() {
        Some(b'5') => {
            rest[1..].iter().any(|&c| c != b'0')
                || digits.last().is_some_and(|&c| (c - b'0') % 2 == 1)
        }
        Some(&c) => c > b'5',
        None => false,
    };
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let split = digits.len() - prec;
    let mut out = String::new();
    // `-0.00` keeps its sign in Python as well, so the sign follows the value, not the digits.
    if x.is_sign_negative() {
        out.push('-');
    }
    out.push_str(&String::from_utf8_lossy(&digits[..split]));
    if prec > 0 {
        out.push('.');
        out.push_str(&String::from_utf8_lossy(&digits[split..]));
    }
    out
}

/// `f'{x:,.{prec}f}'`: [`fixed`] with a comma between each group of three digits before the point.
pub fn commas(x: f64, prec: usize) -> String {
    let s = fixed(x, prec);
    let (sign, rest) = s.strip_prefix('-').map_or(("", s.as_str()), |r| ("-", r));
    let (int_part, frac) = rest
        .split_once('.')
        .map_or((rest, None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// `round(x, n)` for a float: the exact value rounded half to even to `n` places, read back as the nearest float.
pub fn round(x: f64, n: usize) -> f64 {
    fixed(x, n).parse().unwrap_or(x)
}

/// `a // b` for floats (`CPython`'s `float_floor_div`).
pub fn floordiv(a: f64, b: f64) -> f64 {
    let m = a % b;
    let mut div = (a - m) / b;
    if m != 0.0 && ((b < 0.0) != (m < 0.0)) {
        div -= 1.0;
    }
    if div == 0.0 {
        return 0.0_f64.copysign(a / b);
    }
    let mut floor = div.floor();
    if div - floor > 0.5 {
        floor += 1.0;
    }
    floor
}

/// `shlex.quote(s)`: as is when every character is safe, else in single quotes with each `'` as `'"'"'`.
pub fn shell_quote(s: &str) -> String {
    let safe = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-')
    };
    if s.is_empty() {
        return "''".to_owned();
    }
    if s.chars().all(safe) {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `str(x)` of a number from a settings file: an integer as it is, a float as `repr` (shortest digits, `.0` when whole).
pub fn number_str(n: &serde_json::Number) -> String {
    if n.is_f64() {
        crate::secrets::pyjson::dumps(&serde_json::Value::Number(n.clone()))
    } else {
        n.to_string()
    }
}

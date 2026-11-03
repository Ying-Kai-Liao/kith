//! Reading a service's command line back: the Windows Run value, and the words in front of the host when the holder
//! supervises it. Pure text, so it compiles (and its tests run) on every OS; only `windows.rs` reads a real Run key.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::PathBuf;

/// `    KithServe    REG_SZ    "C:\…\kith-servew.exe" serve --port 19222` → its parts.
pub(super) fn parse_run_value(query: &str) -> Option<(PathBuf, Vec<String>)> {
    let value = query.lines().find_map(|line| Some(line.split_once("REG_SZ")?.1.trim()))?;
    let mut words = split_words(value).into_iter();
    let program = words.next()?;
    Some((program.into(), words.collect()))
}

/// Splits on spaces, a double-quoted run being one word (no escapes: Windows paths cannot hold a quote).
pub(super) fn split_words(text: &str) -> Vec<String> {
    let (mut words, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if std::mem::take(&mut any) {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(word);
    }
    words
}

/// The host program in a job's command line: the word after `--` when the holder runs it, else the first word.
pub(super) fn host_program(words: &[String]) -> Option<&String> {
    match words.iter().position(|w| w == "--") {
        Some(i) => words.get(i + 1),
        None => words.first(),
    }
}

/// The holder folder a supervising holder was started with: the word after `--dir`, before the `--` that starts the
/// host. `None` for a job that is not a supervising holder.
pub(super) fn holder_dir(words: &[String]) -> Option<&String> {
    let before = words.iter().position(|w| w == "--").unwrap_or(words.len());
    let at = words[..before].iter().position(|w| w == "--dir")?;
    words[..before].get(at + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_run_value() {
        let query = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    KithServe    REG_SZ    \"C:\\Program Files\\Kith\\kith-servew.exe\" serve --port 19222\r\n\r\n";
        let (program, args) = parse_run_value(query).unwrap();
        assert_eq!(program, PathBuf::from("C:\\Program Files\\Kith\\kith-servew.exe"));
        assert_eq!(args, ["serve", "--port", "19222"]);
    }

    #[test]
    fn keeps_unquoted_words() {
        assert_eq!(split_words("a bb  ccc"), ["a", "bb", "ccc"]);
        assert_eq!(split_words("\"a b\" c\"d e\" \"\" f"), ["a b", "cd e", "", "f"]);
        assert_eq!(split_words("   "), Vec::<String>::new());
    }

    #[test]
    fn reads_a_run_value_with_the_holder_in_front() {
        let query = "    KithServe    REG_SZ    \"C:\\K it\\kith-holder.exe\" \"supervise\" \"--dir\" \"C:\\a b\\holder\" \"--secret-file\" \"C:\\a b\\holder.secret\" \"--\" \"C:\\K it\\kith-servew.exe\" serve --port 19222\r\n";
        let (program, args) = parse_run_value(query).unwrap();
        assert_eq!(program, PathBuf::from("C:\\K it\\kith-holder.exe"));
        assert_eq!(holder_dir(&args).unwrap(), "C:\\a b\\holder");
        assert_eq!(host_program(&args).unwrap(), "C:\\K it\\kith-servew.exe");
    }

    #[test]
    fn a_plain_job_has_no_holder_dir() {
        let words: Vec<String> = ["serve", "--port", "19222"].map(String::from).to_vec();
        assert!(holder_dir(&words).is_none());
        // A `--dir` after the host's `--` belongs to the host.
        let words: Vec<String> = ["supervise", "--", "host", "--dir", "x"].map(String::from).to_vec();
        assert!(holder_dir(&words).is_none());
    }
}

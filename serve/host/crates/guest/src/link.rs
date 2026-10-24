//! The invite link (section 5.2): an https URL with the secrets in the `#` fragment, or the same data as `kith://bot?...` (`kith-dev://` on a dev build).
//! The host's signing key travels in the link, so the client can pin it: `computerId` must be the hash of `k`.

use kith_wire::crypto::{computer_id, unb64_n};
use url::Url;

use crate::exit::CliError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    /// `https://cloud.example`, no trailing slash.
    pub cloud: String,
    pub computer_id: String,
    pub invite_id: String,
    pub token: String,
    /// The computer's Ed25519 key, b64url.
    pub host_sign_key: String,
}

fn bad(why: &str) -> CliError {
    CliError::invite(format!("That isn't a valid Kith invite link: {why}"))
}

fn is_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `http` is for a local or self-hosted dev cloud on this machine only; everything else must be https.
fn cloud_base(u: &Url) -> Result<String, CliError> {
    let host = u.host_str().ok_or_else(|| bad("no host"))?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    match u.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err(bad("the cloud address must be https")),
    }
    let mut base = format!("{}://{host}", u.scheme());
    if let Some(port) = u.port() {
        base.push(':');
        base.push_str(&port.to_string());
    }
    Ok(base)
}

pub fn parse(link: &str) -> Result<Invite, CliError> {
    let link = link.trim().trim_matches(|c| c == '\'' || c == '"');
    let url = Url::parse(link).map_err(|_| bad("it isn't a URL"))?;
    let (cloud, computer_id_arg, invite_id, token, k) = if matches!(url.scheme(), "kith" | "kith-dev") {
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        let get = |key: &str| q.get(key).cloned().ok_or_else(|| bad(&format!("`{key}` is missing")));
        let r = Url::parse(&get("r")?).map_err(|_| bad("`r` isn't a URL"))?;
        (cloud_base(&r)?, get("c")?, get("i")?, get("t")?, get("k")?)
    } else {
        let segs: Vec<&str> = url.path_segments().map(Iterator::collect).unwrap_or_default();
        let at = segs.iter().rposition(|s| *s == "b").filter(|&i| i + 3 == segs.len());
        let Some(i) = at else { return Err(bad("the path should end in /b/<computer>/<invite>")) };
        let (c, inv) = (segs[i + 1], segs[i + 2]);
        let fragment = url.fragment().unwrap_or_default();
        let frag: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(fragment.as_bytes()).into_owned().collect();
        let (Some(t), Some(k)) = (frag.get("t"), frag.get("k")) else {
            return Err(bad(
                "the part after `#` is missing. Paste the whole link, in single quotes so the shell keeps the `#` and `&`",
            ));
        };
        // A cloud served under a path prefix keeps it.
        let prefix = segs[..i].join("/");
        let mut base = cloud_base(&url)?;
        if !prefix.is_empty() {
            base.push('/');
            base.push_str(&prefix);
        }
        (base, c.to_string(), inv.to_string(), t.clone(), k.clone())
    };
    if !is_id(&computer_id_arg) || !is_id(&invite_id) {
        return Err(bad("the computer or invite id is malformed"));
    }
    if token.is_empty() || token.len() > 256 || token.chars().any(char::is_whitespace) {
        return Err(bad("the token is malformed"));
    }
    let key: [u8; 32] = unb64_n(&k).map_err(|_| bad("the computer key is malformed"))?;
    // The computer id is a one-way hash of the key: a link whose two disagree was damaged or forged.
    if computer_id(&key) != computer_id_arg {
        return Err(bad("the computer key doesn't match the computer id"));
    }
    Ok(Invite { cloud, computer_id: computer_id_arg, invite_id, token, host_sign_key: k })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kith_wire::crypto::b64;

    const KEY: [u8; 32] = [7; 32];

    fn cid() -> String {
        computer_id(&KEY)
    }

    fn https(cid_part: &str) -> String {
        format!("https://cloud.example.com/b/{cid_part}/bi_abc123#t=tok-en_1&k={}", b64(&KEY))
    }

    #[test]
    fn https_form() {
        let inv = parse(&https(&cid())).unwrap();
        assert_eq!(inv.cloud, "https://cloud.example.com");
        assert_eq!(inv.computer_id, cid());
        assert_eq!(inv.invite_id, "bi_abc123");
        assert_eq!(inv.token, "tok-en_1");
        assert_eq!(inv.host_sign_key, b64(&KEY));
    }

    #[test]
    fn surrounding_quotes_are_dropped() {
        let quoted = format!("'{}'", https(&cid()));
        assert_eq!(parse(&quoted).unwrap().invite_id, "bi_abc123");
    }

    #[test]
    fn kith_form() {
        let link =
            format!("kith://bot?c={}&i=bi_abc123&t=tok&k={}&r=https%3A%2F%2Fcloud.example.com", cid(), b64(&KEY));
        let inv = parse(&link).unwrap();
        assert_eq!(inv.cloud, "https://cloud.example.com");
        assert_eq!(inv.token, "tok");
    }

    #[test]
    fn kith_dev_form() {
        let link =
            format!("kith-dev://bot?c={}&i=bi_abc123&t=tok&k={}&r=https%3A%2F%2Fcloud.example.com", cid(), b64(&KEY));
        assert_eq!(parse(&link).unwrap().token, "tok");
    }

    #[test]
    fn computer_id_must_hash_the_key() {
        let e = parse(&https("AAAAAAAAAAAAAAAAAAAAAA")).unwrap_err();
        assert_eq!(e.exit, crate::exit::Exit::Invite);
        assert!(e.message.contains("doesn't match"));
    }

    #[test]
    fn a_link_cut_at_the_fragment_says_so() {
        let e = parse(&format!("https://cloud.example.com/b/{}/bi_abc123", cid())).unwrap_err();
        assert!(e.message.contains("after `#`"));
    }

    #[test]
    fn plain_http_only_for_loopback() {
        let link = format!("http://127.0.0.1:8787/b/{}/bi_abc123#t=x&k={}", cid(), b64(&KEY));
        assert_eq!(parse(&link).unwrap().cloud, "http://127.0.0.1:8787");
        let remote = format!("http://cloud.example.com/b/{}/bi_abc123#t=x&k={}", cid(), b64(&KEY));
        assert!(parse(&remote).is_err());
    }

    #[test]
    fn garbage_is_an_invite_error() {
        for s in ["", "hello", "https://cloud.example.com/", "https://cloud.example.com/b/x#t=1&k=2"] {
            assert_eq!(parse(s).unwrap_err().exit, crate::exit::Exit::Invite, "{s}");
        }
    }

    #[test]
    fn key_must_be_32_bytes() {
        let link = format!("https://cloud.example.com/b/{}/bi_abc123#t=x&k=AAAA", cid());
        assert!(parse(&link).is_err());
    }
}

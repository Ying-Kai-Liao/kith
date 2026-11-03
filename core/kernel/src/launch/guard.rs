//! What a run may not touch, as permission rules: the part of `kernel/lib/kith_guard.py` that a launch writes into the
//! settings (the secret paths, rule W, and the must-ask rules). The judging half of that file (`must_ask`, the grants of
//! an escape card) stays in Python until the host moves.

use std::fs;
use std::path::Path;

use crate::paths::realpath;

/// Under the Kith state folder and the machine folder: whole folders that hold logins, tokens and every audience's index.
const SECRET_DIRS: [&str; 7] = [
    "secrets",
    "serve",
    "browsers",
    "bridge",
    "index",
    "room-ledger",
    "core",
];
/// The same places as permission-rule patterns (Read/Edit), plus the database files that sit among them.
pub const SECRET_RULES: [&str; 10] = [
    "secrets/**",
    "serve/token",
    "serve/identity.json",
    "serve/*.db",
    "serve/*.db-*",
    "browsers/**",
    "bridge/**",
    "index/**",
    "memory.db*",
    "core/**",
];
const IN_FOLDER_TREES: [&str; 4] = [".claude", ".vscode", ".idea", ".git/hooks"];
const IN_FOLDER_FILES: [&str; 6] = [
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".mcp.json",
    ".envrc",
    ".git/config",
    ".gitmodules",
];
const HOME_TREES: [&str; 5] = [
    ".config/git",
    "Library/LaunchAgents",
    ".local/bin",
    ".npm-global",
    ".cargo/bin",
];
const HOME_FILES: [&str; 9] = [
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".gitconfig",
    ".claude.json",
];
const ABS_TREES: [&str; 2] = ["/opt/homebrew", "/usr/local/bin"];
const HOME_SECRETS: [&str; 8] = [
    ".ssh/id_*",
    ".aws/credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".docker/config.json",
    ".config/gh/hosts.yml",
    ".gnupg/**",
];

/// `list(dict.fromkeys(items))`.
pub fn dedup<I: IntoIterator<Item = String>>(items: I) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for x in items {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// A plain host name (no wildcard, no address, no port): `kith_guard.valid_domain`.
pub fn valid_domain(d: &str) -> bool {
    let d = d.strip_suffix('\n').unwrap_or(d);
    let label = |l: &str, first_alpha: bool| {
        let b = l.as_bytes();
        let ok = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
        !b.is_empty()
            && b.len() <= 63
            && if first_alpha {
                b[0].is_ascii_lowercase()
            } else {
                ok(b[0])
            }
            && ok(b[b.len() - 1])
            && b.iter().all(|c| ok(*c) || *c == b'-')
    };
    let parts: Vec<&str> = d.split('.').collect();
    (1..=253).contains(&d.len())
        && parts.len() >= 2
        && parts[..parts.len() - 1].iter().all(|l| label(l, false))
        && label(parts[parts.len() - 1], true)
}

/// `both`: the path and its resolved spelling.
pub fn both(p: &str) -> Vec<String> {
    dedup([p.to_owned(), s(&realpath(Path::new(p)))])
}

/// Absolute folders (denyRead / denyWrite) for the secrets: `roots` are the state and machine folders.
pub fn secret_paths(roots: &[String], secrets_dir: &str, machine: &str) -> Vec<String> {
    let mut out = vec![];
    for r in dedup(roots.iter().flat_map(|r| both(r))) {
        out.extend(SECRET_DIRS.iter().map(|f| format!("{r}/{f}")));
    }
    let machine = both(machine);
    for p in both(secrets_dir) {
        if machine.contains(&p) {
            out.push(format!("{p}/env"));
            out.push(format!("{p}/auth.json"));
        } else {
            out.push(p);
        }
    }
    dedup(out)
}

/// `(trees, files)` the run may not write: below each of `folders`, below `home`, the PATH folders, and the extra
/// `trees` (the kernel, the agent's package, ...). Both spellings of each.
pub fn rule_w(home: &str, folders: &[String], trees: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut t, mut f) = (vec![], vec![]);
    for d in folders {
        for x in both(d) {
            // at any depth: a repository cloned or created below a folder the run may write in
            for pre in [format!("{x}/"), format!("{x}/**/")] {
                t.extend(IN_FOLDER_TREES.iter().map(|n| format!("{pre}{n}")));
                f.extend(IN_FOLDER_FILES.iter().map(|n| format!("{pre}{n}")));
            }
        }
    }
    for x in both(home) {
        t.extend(HOME_TREES.iter().map(|n| format!("{x}/{n}")));
        f.extend(HOME_FILES.iter().map(|n| format!("{x}/{n}")));
    }
    for a in ABS_TREES {
        t.extend(both(a));
    }
    for p in trees {
        t.extend(both(p));
    }
    (dedup(t), dedup(f))
}

/// Edit deny rules (which cover every tool that writes a file) for a rule W list.
pub fn edit_rules(trees: &[String], files: &[String]) -> Vec<String> {
    trees
        .iter()
        .map(|t| format!("Edit(/{t}/**)"))
        .chain(files.iter().map(|f| format!("Edit(/{f})")))
        .collect()
}

// ---- must ask

const SECRET_CMDS: [&str; 22] = [
    "security find-generic-password",
    "security find-internet-password",
    "security dump-keychain",
    "security add-generic-password",
    "security add-internet-password",
    "security delete-generic-password",
    "security delete-internet-password",
    "gh auth login",
    "gh auth token",
    "gh auth refresh",
    "gcloud auth login",
    "gcloud auth print-access-token",
    "gcloud auth print-identity-token",
    "gcloud auth application-default",
    "npm login",
    "npm adduser",
    "npm token",
    "docker login",
    "aws configure",
    "wrangler login",
    "kith-auth set",
    "kith-auth get",
];
const SECRET_CMDS_LAST: &str = "kith-auth once";
const TRUST_CMDS: [&str; 11] = [
    "kith allow",
    "kith-allow",
    "kith-auth grant",
    "kith-auth revoke",
    "kith-auth person",
    "kith-auth member",
    "kith-auth bot",
    "kith-auth forget",
    "kith-auth access",
    "launchctl load",
    "launchctl bootstrap",
];
const DESTROY_CMDS: [&str; 10] = [
    "shred",
    "srm",
    "mkfs",
    "diskutil erase",
    "diskutil partitionDisk",
    "diskutil zeroDisk",
    "diskutil secureErase",
    "diskutil apfs delete",
    "git reset --hard",
    "git clean",
];
const OUTWARD_CMDS: [&str; 24] = [
    "gh pr merge",
    "gh repo delete",
    "gh release create",
    "gh release delete",
    "gh release upload",
    "gh release edit",
    "gh gist create",
    "npm publish",
    "pnpm publish",
    "yarn publish",
    "yarn npm publish",
    "cargo publish",
    "uv publish",
    "poetry publish",
    "twine upload",
    "gem push",
    "docker push",
    "wrangler deploy",
    "wrangler publish",
    "netlify deploy",
    "vercel deploy",
    "fly deploy",
    "sendmail",
    "mail",
];
const CODE_CMDS: [&str; 13] = [
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "pnpm dlx",
    "yarn dlx",
    "npm exec",
    "npm x",
    "bun x",
    "pipx run",
    "uv tool run",
    "eval",
    "deno eval",
];
/// Interpreters and the flags that hand them code inline.
const INLINE: [(&str, &[&str]); 12] = [
    ("python", &["-c"]),
    ("node", &["-e", "--eval", "-p", "--print"]),
    ("deno", &["eval"]),
    ("bun", &["-e", "--eval", "eval"]),
    ("ruby", &["-e"]),
    ("perl", &["-e", "-E"]),
    ("php", &["-r"]),
    ("osascript", &["-e"]),
    ("bash", &["-c", "-s"]),
    ("sh", &["-c", "-s"]),
    ("zsh", &["-c", "-s"]),
    ("dash", &["-c", "-s"]),
];
const INLINE_FISH: (&str, &[&str]) = ("fish", &["-c"]);
const SHELLS: [&str; 5] = ["bash", "sh", "zsh", "dash", "fish"];

/// One sentence per class for the auto-mode classifier (`autoMode.soft_deny`), for what the rules cannot spell.
pub const SOFT_DENY: [&str; 5] = [
    "Never read, print or send a password, token, key or login (Keychain, kith-auth values, ~/.ssh, cloud logins).",
    "Never change a file that decides what later runs may do (any allowed.json, settings.json, capabilities.yaml, run/once, the agent's own package outside skills/learned, kith's protected paths, shell start-up files, LaunchAgents, folders on PATH) without the owner's yes.",
    "Never delete or overwrite anything outside the run's own writable folders and its working copy (rm -rf, git reset --hard, git clean, a disk tool) without the owner's yes.",
    "Never do something public or past the owner without the owner's yes: a force push or a push to main or master, a merge, a release, a publish or deploy, a network post, a message to anyone but the owner, spending money.",
    "Never run code that has no name: an interpreter or shell given inline code, a package fetched and run (npx, uvx, dlx), eval, or a download piped into a shell.",
];

/// Where the trust and secret paths are for one run: `Places` of `kith_guard.py`.
pub struct Places {
    pub home: String,
    pub state: Vec<String>,
    pub machine: Vec<String>,
    pub agents: Vec<String>,
    pub repos: Vec<String>,
    pub kernel: Vec<String>,
    pub package: Vec<String>,
}

impl Places {
    /// `state`, `machine`, `agents` and `kith_home` as the paths rules name them; `kernel` is the kernel folder
    /// (`kith_paths.KERNEL`, resolved).
    pub fn new(
        home: &str,
        state: &str,
        machine: &str,
        agents: &str,
        kith_home: &str,
        kernel: &Path,
        package: &str,
    ) -> Self {
        let state_both = both(state);
        let roots: Vec<String> = std::iter::once(agents.to_owned())
            .chain(state_both.iter().map(|x| format!("{x}/agents")))
            .collect();
        let agents = dedup(roots.iter().flat_map(|r| both(r)));
        let kernel_real = realpath(kernel);
        let mut repos = vec![kith_home.to_owned()];
        if kernel_real.file_name().is_some_and(|n| n == "kernel")
            && let Some(p) = kernel_real.parent()
        {
            repos.push(s(p));
        }
        let repos = dedup(
            repos
                .iter()
                .flat_map(|r| both(r))
                .filter(|r| Path::new(r).join(".github/kith-protected-paths").is_file()),
        );
        Places {
            home: home.to_owned(),
            state: state_both,
            machine: both(machine),
            agents,
            repos,
            kernel: both(&s(&kernel_real)),
            package: both(package),
        }
    }

    /// `(trees, files)` of the repo's protected paths, under each live checkout of the kith repo.
    fn protected(&self) -> (Vec<String>, Vec<String>) {
        let (mut t, mut f) = (vec![], vec![]);
        for r in &self.repos {
            let Ok(text) = fs::read(Path::new(r).join(".github/kith-protected-paths")) else {
                continue;
            };
            for line in String::from_utf8_lossy(&text).lines() {
                let line = line.trim();
                if line.is_empty()
                    || line.starts_with('#')
                    || line.contains("..")
                    || line.starts_with('/')
                {
                    continue;
                }
                let path = format!("{r}/{}", line.trim_end_matches('/'));
                if line.ends_with('/') {
                    t.push(path);
                } else {
                    f.push(path);
                }
            }
        }
        (t, f)
    }

    /// `(trees, files)`: absolute trees and files that decide what later runs may do.
    fn trust(&self) -> (Vec<String>, Vec<String>) {
        let (mut trees, mut files) = self.protected();
        for h in both(&self.home) {
            files.push(format!("{h}/.claude/settings.json"));
            files.push(format!("{h}/.claude/settings.local.json"));
            files.extend(HOME_FILES.iter().map(|n| format!("{h}/{n}")));
            trees.extend(HOME_TREES.iter().map(|n| format!("{h}/{n}")));
        }
        for d in self.state.iter().chain(&self.machine) {
            files.push(format!("{d}/settings.json"));
        }
        for m in &self.machine {
            trees.push(format!("{m}/run/once"));
            trees.push(format!("{m}/core"));
        }
        for a in ABS_TREES {
            trees.extend(both(a));
        }
        trees.extend(self.package.iter().cloned());
        (dedup(trees), dedup(files))
    }
}

/// Bash rules for command prefixes: the bare name, git's -C form, and kith's tools by their path in each kernel.
fn bash_rules(cmds: &[&str], kernel: &[String]) -> Vec<String> {
    let mut out = vec![];
    for c in cmds {
        let w: Vec<&str> = c.split_whitespace().collect();
        let tail = w[1..].join(" ");
        let mut names = vec![w[0].to_owned()];
        if w[0] == "kith-auth" {
            names.push("hq-auth".to_owned());
            out.push(format!("Bash(kith auth {tail}:*)"));
            out.push(format!("Bash(hq auth {tail}:*)"));
        }
        if matches!(w[0], "kith-auth" | "kith-allow" | "kith") {
            names.extend(kernel.iter().map(|k| format!("{k}/bin/{}", w[0])));
            names.extend(kernel.iter().map(|k| format!("\"{k}/bin/{}\"", w[0])));
        }
        if w[0] == "kith" && w.get(1) == Some(&"allow") {
            out.push("Bash(hq allow:*)".to_owned());
        }
        for n in &names {
            let full = std::iter::once(n.as_str())
                .chain(w[1..].iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            out.push(format!("Bash({full}:*)"));
        }
        if w[0] == "git" {
            out.push(format!("Bash(git -C * {tail}*)"));
        }
    }
    out
}

fn is_dir(p: &str) -> bool {
    Path::new(p).is_dir()
}

fn sorted_names(p: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(p)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// What `must_ask_rules` gives: secrets are denied, the rest ask.
pub struct MustAsk {
    pub deny: Vec<String>,
    pub ask_files: Vec<String>,
    pub ask_bash: Vec<String>,
}

pub fn must_ask_rules(places: &Places) -> MustAsk {
    let (mut deny, mut ask_files, mut ask_bash) = (vec![], vec![], vec![]);
    for h in both(&places.home) {
        deny.extend(HOME_SECRETS.iter().map(|x| format!("Read(/{h}/{x})")));
        deny.extend(HOME_SECRETS.iter().map(|x| format!("Edit(/{h}/{x})")));
    }
    let secret_cmds: Vec<&str> = SECRET_CMDS
        .iter()
        .copied()
        .chain([SECRET_CMDS_LAST])
        .collect();
    deny.extend(bash_rules(&secret_cmds, &places.kernel));
    let (trees, files) = places.trust();
    for a in &places.agents {
        for n in [
            "role/settings.json",
            "role/capabilities.yaml",
            "role/mcp.json",
            "allowed.json",
        ] {
            ask_files.push(format!("Edit(/{a}/*/{n})"));
        }
    }
    for pk in &places.package {
        ask_files.push(format!("Edit(/{pk}/*)"));
        for n in sorted_names(pk) {
            let sub = format!("{pk}/{n}");
            if n == "skills" && is_dir(&sub) {
                ask_files.push(format!("Edit(/{pk}/skills/*)"));
                for k in sorted_names(&sub) {
                    if k != "learned" && is_dir(&format!("{sub}/{k}")) {
                        ask_files.push(format!("Edit(/{pk}/skills/{k}/**)"));
                    }
                }
            } else if is_dir(&sub) {
                ask_files.push(format!("Edit(/{pk}/{n}/**)"));
            }
        }
    }
    ask_files.extend(
        trees
            .iter()
            .filter(|t| !places.package.contains(t))
            .map(|t| format!("Edit(/{t}/**)")),
    );
    ask_files.extend(files.iter().map(|f| format!("Edit(/{f})")));
    ask_bash.extend(bash_rules(&TRUST_CMDS, &places.kernel));
    // destroy: the disk tools and git's discards; a plain rm is judged by the classifier (soft_deny)
    ask_bash.extend(bash_rules(&DESTROY_CMDS, &[]));
    ask_bash.push("Bash(dd *of=*)".to_owned());
    let homes: Vec<String> = ["~".to_owned(), "$HOME".to_owned()]
        .into_iter()
        .chain(both(&places.home))
        .collect();
    for h in homes {
        ask_bash.push(format!("Bash(rm -* {h})"));
        ask_bash.push(format!("Bash(rm -* {h}/)"));
    }
    ask_bash.push("Bash(rm -* /)".to_owned());
    // outward
    for g in ["git", "git -C *"] {
        for shape in [
            "--force*",
            "-f",
            "-f *",
            "* -f",
            "* -f *",
            "* --force*",
            "* +*",
            "--tags*",
            "* --tags*",
            "--follow-tags*",
            "* --follow-tags*",
            "--mirror*",
            "* --mirror*",
            "* --delete *",
            "* -d *",
            "* main",
            "* main *",
            "* master",
            "* master *",
            "*:main",
            "*:main *",
            "*:master",
            "*:master *",
            "*:refs/heads/main*",
            "*:refs/heads/master*",
            "* refs/tags/*",
        ] {
            ask_bash.push(format!("Bash({g} push {shape})"));
        }
    }
    let outward: Vec<&str> = OUTWARD_CMDS
        .iter()
        .copied()
        .filter(|c| *c != "mail")
        .collect();
    ask_bash.extend(bash_rules(&outward, &[]));
    ask_bash.push("Bash(mail -s *)".to_owned());
    ask_bash.push("Bash(mail * -s *)".to_owned());
    for m in ["POST", "PUT", "PATCH", "DELETE"] {
        for tpl in [
            "Bash(gh api * -X {m}*)",
            "Bash(gh api -X {m}*)",
            "Bash(gh api * --method {m}*)",
            "Bash(gh api --method {m}*)",
            "Bash(curl * -X {m}*)",
            "Bash(curl -X {m}*)",
            "Bash(curl * --request {m}*)",
            "Bash(curl --request {m}*)",
            "Bash(curl *-X{m}*)",
        ] {
            ask_bash.push(tpl.replace("{m}", m));
        }
    }
    for f in ["-f", "-F", "--field", "--raw-field", "--input"] {
        ask_bash.push(format!("Bash(gh api * {f} *)"));
        ask_bash.push(format!("Bash(gh api {f} *)"));
    }
    for f in [
        "-d",
        "--data*",
        "-F",
        "--form*",
        "--json*",
        "-T",
        "--upload-file*",
    ] {
        if f.ends_with('*') {
            ask_bash.push(format!("Bash(curl * {f})"));
            ask_bash.push(format!("Bash(curl {f})"));
        } else {
            ask_bash.push(format!("Bash(curl * {f} *)"));
            ask_bash.push(format!("Bash(curl {f} *)"));
        }
    }
    ask_bash.push("Bash(wget * --post-*)".to_owned());
    ask_bash.push("Bash(wget --post-*)".to_owned());
    // code
    ask_bash.extend(bash_rules(&CODE_CMDS, &[]));
    for (b, flags) in INLINE.iter().chain([&INLINE_FISH]) {
        let b: &str = b;
        let names: Vec<&str> = if b == "python" {
            vec!["python", "python3"]
        } else {
            vec![b]
        };
        for n in names {
            ask_bash.push(format!("Bash({n})"));
            ask_bash.push(format!("Bash({n} -)"));
            ask_bash.push(format!("Bash({n} - *)"));
            ask_bash.extend(flags.iter().map(|f| format!("Bash({n} {f} *)")));
            ask_bash.extend(flags.iter().map(|f| format!("Bash({n} {f})")));
            if SHELLS.contains(&b) {
                // the usual combined spellings; `-*c *` would run on to any later word ending in c
                for f in [
                    "-lc", "-ec", "-xc", "-ic", "-euc", "-exc", "-lic", "-cl", "-ce",
                ] {
                    ask_bash.push(format!("Bash({n} {f} *)"));
                }
            }
        }
    }
    MustAsk {
        deny: dedup(deny),
        ask_files: dedup(ask_files),
        ask_bash: dedup(ask_bash),
    }
}

//! `fetch(home)`: downloads each third-party skill `skills.json` links that is not already there at its pinned commit.

use std::fs;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use super::git::run;
use super::problems::{external_skills, read, source_line};
use super::py::{Kind, PyErr, R, is_file, os_err, rmtree, strip, write_text};
use super::Ctx;

/// The skills of one `(repo, ref)`: `(name, path in the repo)`.
type Group = ((String, String), Vec<(String, String)>);

/// A temporary folder that goes away with it (`tempfile.TemporaryDirectory(prefix='kith-fetch-')`).
pub(super) struct TempDir(pub(super) PathBuf);

impl TempDir {
    pub(super) fn new() -> R<Self> {
        let base = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        for n in 0..100u32 {
            let p = base.join(format!("kith-fetch-{:x}{:x}{n}", std::process::id(), stamp));
            // like mkdtemp: nobody else reads what a fetch puts here
            match fs::DirBuilder::new().mode(0o700).create(&p) {
                Ok(()) => return Ok(Self(p)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(os_err(&e, &p)),
            }
        }
        Err(PyErr::new(Kind::Os, "[Errno 17] File exists: 'kith-fetch-'"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `shutil.copytree(src, dst, ignore=shutil.ignore_patterns('.git'))`: links are followed, `.git` is left out.
fn copytree(src: &Path, dst: &Path) -> R<()> {
    fs::create_dir_all(dst).map_err(|e| os_err(&e, dst))?;
    let rd = fs::read_dir(src).map_err(|e| os_err(&e, src))?;
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for e in entries {
        if e.file_name() == ".git" {
            continue;
        }
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let meta = fs::metadata(&from).map_err(|e| os_err(&e, &from))?;
        if meta.is_dir() {
            copytree(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| os_err(&e, &from))?;
            if let Ok(t) = meta.modified()
                && let Ok(f) = fs::File::options().write(true).open(&to)
            {
                let _ = f.set_modified(t);
            }
        }
    }
    if let Ok(m) = fs::metadata(src) {
        let _ = fs::set_permissions(dst, m.permissions());
    }
    Ok(())
}

fn field<'a>(src: &'a Value, key: &str) -> &'a str {
    src.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The path a linked skill has in its repo: `path.strip('/') or '.'`.
fn path_of(src: &Value) -> String {
    let p = field(src, "path").trim_matches('/');
    if p.is_empty() { ".".to_owned() } else { p.to_owned() }
}

/// Fetches what is missing (or every linked skill, with `force`) and returns the names fetched, sorted.
pub fn fetch(ctx: &Ctx, home: &Path, force: bool) -> R<Vec<String>> {
    let skills = external_skills(home)?;
    let mut todo: Vec<&(String, Value)> = vec![];
    for s in &skills {
        if force || read(&home.join("skills").join(&s.0).join(".source"))? != source_line(&s.1) {
            todo.push(s);
        }
    }
    // (repo, ref) -> [(name, path)], in the order they were first seen
    let mut groups: Vec<Group> = vec![];
    for (name, src) in todo.iter().map(|s| (&s.0, &s.1)) {
        let key = (field(src, "repo").to_owned(), field(src, "ref").to_owned());
        let item = (name.clone(), path_of(src));
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(item),
            None => groups.push((key, vec![item])),
        }
    }
    for ((repo, reference), items) in &groups {
        // A shallow, blob-less, sparse fetch of one commit: only these folders' files come down, however big the repo.
        let tmp = TempDir::new()?;
        let whole = items.iter().any(|(_, p)| p == ".");
        let url = format!("{}/{repo}.git", ctx.github);
        let sparse: Vec<String> = items.iter().map(|(_, p)| format!("/{p}/")).collect();
        let mut steps: Vec<Vec<String>> = vec![
            vec!["init".into(), "-q".into()],
            vec!["remote".into(), "add".into(), "origin".into(), url],
        ];
        if !whole {
            let mut s = vec!["sparse-checkout".to_owned(), "set".into(), "--no-cone".into()];
            s.extend(sparse);
            steps.push(s);
        }
        steps.push(
            ["fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", reference]
                .map(String::from)
                .to_vec(),
        );
        steps.push(vec!["checkout".into(), "-q".into(), "FETCH_HEAD".into()]);
        for step in &steps {
            let mut cmd = Command::new("git");
            cmd.arg("-C")
                .arg(&tmp.0)
                .args(step)
                .env("GIT_TERMINAL_PROMPT", "0");
            let r = run(cmd, Duration::from_secs(300))?;
            if r.code != 0 {
                return Err(PyErr::new(
                    Kind::Runtime,
                    format!(
                        "fetching {repo}@{}: git {} failed: {}",
                        reference.chars().take(12).collect::<String>(),
                        step[0],
                        strip(&r.stderr)
                    ),
                ));
            }
        }
        for (name, path) in items {
            let src = tmp.0.join(path);
            if !is_file(&src.join("SKILL.md")) {
                return Err(PyErr::new(
                    Kind::Runtime,
                    format!(
                        "{repo}@{} has no {path}/SKILL.md",
                        reference.chars().take(12).collect::<String>()
                    ),
                ));
            }
            let dest = home.join("skills").join(name);
            let part = home.join("skills").join(format!(".{name}.part"));
            rmtree(&part);
            copytree(&src, &part)?;
            let link = skills.iter().find(|s| &s.0 == name).map(|s| &s.1);
            write_text(
                &part.join(".source"),
                &format!("{}\n", link.map(source_line).unwrap_or_default()),
            )?;
            rmtree(&dest);
            fs::rename(&part, &dest).map_err(|e| os_err(&e, &part))?;
        }
    }
    if !skills.is_empty() {
        // fetched folders are not part of the repo; rewritten only on change (its mtime is in the fingerprint)
        let ignore = home.join("skills").join(".gitignore");
        let mut names: Vec<&str> = skills.iter().map(|s| s.0.as_str()).collect();
        names.sort_unstable();
        let text = format!(
            "# third-party skills fetched from skills.json (kith-pkg fetch); not committed\n{}",
            names.iter().fold(String::new(), |acc, n| acc + &format!("/{n}/\n"))
        );
        if read(&ignore)? != strip(&text) {
            write_text(&ignore, &text)?;
        }
    }
    let mut got: Vec<String> = todo.iter().map(|s| s.0.clone()).collect();
    got.sort();
    Ok(got)
}

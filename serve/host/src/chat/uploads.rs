//! Files sent with a message. A client uploads each file in chunks (the channel caps a
//! message at 1 MiB), then names the uploads in `send`; the agent gets their paths.

use crate::agent::workspace;
use crate::service;
use crate::store::BotConfig;
use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Largest file accepted.
pub const MAX_FILE: u64 = 100 * 1024 * 1024;

/// Inside a personal workspace, so reading an upload needs no extra approval.
/// A group (no workspace) keeps its members' shared files in its data folder.
pub fn root(cfg: &BotConfig) -> PathBuf {
    if workspace::is_managed(cfg) {
        Path::new(&cfg.cwd).join("uploads")
    } else {
        service::data_dir().join("bots").join(&cfg.id).join("uploads")
    }
}

/// `uploads/<upload id>/`, the id checked so it can't leave the folder.
fn dir(root: &Path, upload: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(upload).context("invalid upload id")?;
    Ok(root.join(upload.to_ascii_lowercase()))
}

/// The file's own name only: no folders, no hidden or empty names.
fn clean_name(name: &str) -> Result<String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if name.is_empty() || name.starts_with('.') || name.len() > 200 {
        bail!("invalid file name");
    }
    Ok(name.to_owned())
}

/// Most bytes one invited person may keep in uploads (all chats, all files), so a guest can't fill the disk.
/// The owner has no limit. A guest already over it keeps their files but can't add more. Checked per chunk against what
/// is on disk, so two chunks racing can overshoot by about a chunk (1 MiB each); that is accepted.
pub const GUEST_QUOTA: u64 = 1024 * 1024 * 1024;

/// Who is uploading: the owner (no limit) or an invited person, whose bytes count against [`GUEST_QUOTA`].
#[derive(Clone, Copy)]
pub enum Uploader<'a> {
    Owner,
    Guest(&'a str),
}

/// Marks who an upload folder belongs to (a hidden file, so `resolve` and file names never meet it).
const OWNER_MARK: &str = ".by";

/// Bytes the guest keeps under `root`: the files of every upload folder marked as theirs.
fn used_by(root: &Path, person: &str) -> u64 {
    let Ok(dirs) = std::fs::read_dir(root) else { return 0 };
    dirs.filter_map(Result::ok)
        .filter(|d| d.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|d| std::fs::read_to_string(d.path().join(OWNER_MARK)).is_ok_and(|by| by == person))
        .filter_map(|d| std::fs::read_dir(d.path()).ok())
        .flat_map(|files| files.filter_map(Result::ok))
        .filter(|f| !f.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|f| f.metadata().ok().filter(std::fs::Metadata::is_file).map(|m| m.len()))
        .sum()
}

/// `uploads/<id>/`, made without following a symlink: the root and the folder must be real folders and the folder must
/// resolve to a direct child of the root, so a link planted there can't send a write elsewhere.
fn safe_dir(root: &Path, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(root).context("creating the upload folder")?;
    if std::fs::symlink_metadata(root).context("checking the upload folder")?.file_type().is_symlink() {
        bail!("the upload folder is a symlink");
    }
    match std::fs::create_dir(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e).context("creating the upload folder"),
        _ => {}
    }
    if std::fs::symlink_metadata(dir).context("checking the upload folder")?.file_type().is_symlink() {
        bail!("the upload folder is a symlink");
    }
    let (real_root, real_dir) = (root.canonicalize()?, dir.canonicalize()?);
    if real_dir.parent() != Some(real_root.as_path()) {
        bail!("the upload folder is outside the uploads folder");
    }
    Ok(())
}

/// Appends one chunk at `offset`. A repeated chunk (a retry) is accepted as is.
/// Returns the attachment once `done`.
pub fn append(
    root: &Path,
    upload: &str,
    name: &str,
    offset: u64,
    data: &[u8],
    done: bool,
    who: Uploader,
) -> Result<Option<Value>> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = dir(root, upload)?;
    let path = dir.join(clean_name(name)?);
    safe_dir(root, &dir)?;
    // A link in the file's place is refused, whatever it points at.
    let (len, exists) = match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.is_file() => bail!("the upload is not a regular file"),
        Ok(m) => (m.len(), true),
        Err(_) => (0, false),
    };
    let end = offset + data.len() as u64;
    if end > MAX_FILE {
        bail!("file is larger than {} MB", MAX_FILE / 1024 / 1024);
    }
    if offset == len {
        if let Uploader::Guest(person) = who
            && used_by(root, person) + data.len() as u64 > GUEST_QUOTA
        {
            bail!("Upload limit reached: you can keep up to {} MB of files here", GUEST_QUOTA / 1024 / 1024);
        }
        let mut opts = std::fs::OpenOptions::new();
        // Windows has no O_NOFOLLOW; its links need a privilege the host does not run with (windows-owner).
        #[cfg(unix)]
        opts.custom_flags(libc::O_NOFOLLOW);
        let created = !exists;
        let mut file = if created { opts.write(true).create_new(true) } else { opts.append(true) }
            .open(&path)
            .context("opening the upload")?;
        if let (true, Uploader::Guest(person)) = (created, who) {
            let _ = std::fs::write(dir.join(OWNER_MARK), person);
        }
        if let Err(e) = file.write_all(data) {
            // Roll back the partial chunk so it isn't counted or half-kept.
            if created {
                let _ = std::fs::remove_file(&path);
            } else {
                let _ = file.set_len(len);
            }
            return Err(e).context("writing the upload");
        }
    } else if end != len {
        bail!("upload out of order: at {len}, got {offset}");
    }
    Ok(done.then(|| json!({"id": upload, "name": path.file_name().map(|n| n.to_string_lossy()), "size": end})))
}

/// A finished upload: its path and chat metadata.
pub fn resolve(root: &Path, upload: &str) -> Result<(PathBuf, Value)> {
    let dir = dir(root, upload)?;
    let file = std::fs::read_dir(&dir)
        .context("unknown upload")?
        .filter_map(Result::ok)
        .find(|e| e.file_type().is_ok_and(|t| t.is_file()) && !e.file_name().to_string_lossy().starts_with('.'))
        .context("empty upload")?;
    let size = file.metadata().map_or(0, |m| m.len());
    let name = file.file_name().to_string_lossy().into_owned();
    Ok((file.path(), json!({"id": upload, "name": name, "size": size})))
}

/// Where a sent attachment (`{id, name}` chat metadata) lives, without touching the disk.
pub fn path_of(root: &Path, meta: &Value) -> Option<PathBuf> {
    let name = clean_name(meta["name"].as_str()?).ok()?;
    Some(dir(root, meta["id"].as_str()?).ok()?.join(name))
}

/// Up to `len` bytes of a finished upload from `offset`, and its full size.
pub fn read(root: &Path, upload: &str, offset: u64, len: usize) -> Result<(Vec<u8>, u64)> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (path, _) = resolve(root, upload)?;
    let mut file = std::fs::File::open(&path).context("opening the upload")?;
    let size = file.metadata().context("reading the upload")?.len();
    file.seek(SeekFrom::Start(offset)).context("reading the upload")?;
    let mut data = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut data).context("reading the upload")?;
    Ok((data, size))
}

/// What the agent reads after the message text.
pub fn prompt_suffix(paths: &[PathBuf]) -> String {
    let list: Vec<String> = paths.iter().map(|p| format!("- {}", p.display())).collect();
    format!("\n\n[Attached files — read them from these paths]\n{}", list.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(clean_name("../../etc/passwd").expect("last component"), "passwd");
        assert_eq!(clean_name("C:\\x\\photo.jpg").expect("last component"), "photo.jpg");
        assert!(clean_name("..").is_err());
        assert!(clean_name(".ssh").is_err());
        assert!(clean_name("dir/").is_err());
    }

    #[test]
    fn chunks_append_and_retries_are_idempotent() {
        let root = std::env::temp_dir().join(format!("kith-uploads-{}", uuid::Uuid::new_v4()));
        let root = root.as_path();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(dir(root, "../x").is_err());
        assert!(append(root, &id, "a.txt", 0, b"hello ", false, Uploader::Owner).expect("first").is_none());
        assert!(append(root, &id, "a.txt", 0, b"hello ", false, Uploader::Owner).is_ok(), "retry of the first chunk");
        assert!(append(root, &id, "a.txt", 3, b"xx", false, Uploader::Owner).is_err(), "out of order");
        let done = append(root, &id, "a.txt", 6, b"world", true, Uploader::Owner).expect("last").expect("done");
        assert_eq!(done["size"], 11);
        let (path, meta) = resolve(root, &id).expect("resolve");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "hello world");
        assert_eq!(meta["name"], "a.txt");
        assert_eq!(read(root, &id, 6, 100).expect("read"), (b"world".to_vec(), 11));
        assert!(path.starts_with(root));
        assert_eq!(path_of(root, &done), Some(path));
        let _ = std::fs::remove_dir_all(root);
    }

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("kith-uploads-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn a_guest_over_quota_is_refused_and_the_owner_is_not() {
        let root = tmp();
        let id = uuid::Uuid::new_v4().to_string();
        // Fill the guest's quota with a sparse file, as if a past upload had kept that much.
        append(&root, &id, "big.bin", 0, b"x", false, Uploader::Guest("p1")).expect("first");
        let f = std::fs::OpenOptions::new().write(true).open(root.join(&id).join("big.bin")).expect("open");
        f.set_len(GUEST_QUOTA).expect("size");
        let other = uuid::Uuid::new_v4().to_string();
        let err = append(&root, &other, "a.txt", 0, b"hi", true, Uploader::Guest("p1")).expect_err("over");
        assert!(err.to_string().starts_with("Upload limit reached"), "{err}");
        assert!(append(&root, &other, "a.txt", 0, b"hi", true, Uploader::Guest("p2")).is_ok(), "another person");
        let third = uuid::Uuid::new_v4().to_string();
        assert!(append(&root, &third, "a.txt", 0, b"hi", true, Uploader::Owner).is_ok(), "owner has no quota");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_or_folder_is_refused() {
        let root = tmp();
        let outside = tmp();
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::create_dir_all(&root).expect("root");
        // A link where the upload folder goes.
        let id = uuid::Uuid::new_v4().to_string();
        std::os::unix::fs::symlink(&outside, root.join(&id)).expect("link");
        assert!(append(&root, &id, "a.txt", 0, b"x", true, Uploader::Owner).is_err());
        // A link where the file goes, dangling or not.
        let id = uuid::Uuid::new_v4().to_string();
        std::fs::create_dir_all(root.join(&id)).expect("dir");
        let target = outside.join("victim");
        std::os::unix::fs::symlink(&target, root.join(&id).join("a.txt")).expect("link");
        assert!(append(&root, &id, "a.txt", 0, b"x", true, Uploader::Owner).is_err());
        assert!(!target.exists(), "nothing was written through the link");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}

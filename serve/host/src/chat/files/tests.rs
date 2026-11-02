use super::*;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kith-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn snapshots_preserve_binary_empty_and_hidden_files_after_source_changes() {
    let temp = Temp::new();
    for (name, data) in
        [("report.bin", vec![0, 255, 1, 128]), (".hidden", vec![]), ("no-extension", vec![42; CHUNK_SIZE + 13])]
    {
        std::fs::write(temp.0.join(name), &data).unwrap();
        let root = temp.0.join("snapshots");
        let mut saved = snapshot(&root, &temp.0, name, None, None, &AtomicBool::new(false)).unwrap();
        saved.commit();
        std::fs::remove_file(temp.0.join(name)).unwrap();
        let mut downloaded = read(&root, &saved.meta, 0).unwrap();
        if downloaded.len() < data.len() {
            downloaded.extend(read(&root, &saved.meta, CHUNK_SIZE as u64).unwrap());
        }
        assert_eq!(downloaded, data);
        assert_eq!(saved.meta.name, name);
        assert_eq!(saved.meta.size, data.len() as u64);
        assert_eq!(read(&root, &saved.meta, data.len() as u64).unwrap(), Vec::<u8>::new());
        assert!(read(&root, &saved.meta, data.len() as u64 + 1).is_err());
    }
}

#[test]
fn failures_and_unpublished_snapshots_leave_no_files() {
    let temp = Temp::new();
    let root = temp.0.join("snapshots");
    std::fs::write(temp.0.join("file"), [1]).unwrap();
    assert!(snapshot(&root, &temp.0, "missing", None, None, &AtomicBool::new(false)).is_err());
    assert!(snapshot(&root, &temp.0, ".", None, None, &AtomicBool::new(false)).is_err());
    assert!(snapshot(&root, &temp.0, "file", Some("../escape"), None, &AtomicBool::new(false)).is_err());
    assert!(snapshot(&root, &temp.0, "file", None, None, &AtomicBool::new(true)).is_err());
    let saved = snapshot(&root, &temp.0, "file", None, None, &AtomicBool::new(false)).unwrap();
    drop(saved);
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn rejects_large_files_and_damaged_snapshots() {
    let temp = Temp::new();
    let root = temp.0.join("snapshots");
    let file = File::create(temp.0.join("large")).unwrap();
    file.set_len(MAX_FILE).unwrap();
    let exact = snapshot(&root, &temp.0, "large", None, None, &AtomicBool::new(false)).unwrap();
    assert_eq!(exact.meta.size, MAX_FILE);
    drop(exact);
    file.set_len(MAX_FILE + 1).unwrap();
    assert!(snapshot(&root, &temp.0, "large", None, None, &AtomicBool::new(false)).is_err());
    std::fs::write(temp.0.join("file"), [42]).unwrap();
    let saved = snapshot(&root, &temp.0, "file", None, None, &AtomicBool::new(false)).unwrap();
    std::fs::write(&saved.path, []).unwrap();
    assert!(read(&root, &saved.meta, 0).is_err());
    assert!(path(&root, "../file").is_err());
}

#[cfg(unix)]
#[test]
fn snapshot_reads_symlink_sources_but_never_symlink_storage_or_special_files() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let root = temp.0.join("snapshots");
    std::fs::write(temp.0.join("file"), [42]).unwrap();
    symlink(temp.0.join("file"), temp.0.join("link")).unwrap();
    let saved = snapshot(&root, &temp.0, "link", Some("linked.dat"), None, &AtomicBool::new(false)).unwrap();
    std::fs::remove_file(&saved.path).unwrap();
    symlink(temp.0.join("file"), &saved.path).unwrap();
    assert!(read(&root, &saved.meta, 0).is_err());
    assert!(snapshot(&root, &temp.0, "/dev/null", Some("device"), None, &AtomicBool::new(false)).is_err());
}

#[test]
fn cancellation_during_copy_stops_before_reading_the_next_chunk() {
    struct CancellingReader<'a>(&'a AtomicBool, usize);
    impl std::io::Read for CancellingReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.1 += 1;
            self.0.store(true, Ordering::Release);
            buffer[0] = 42;
            Ok(1)
        }
    }
    let cancelled = AtomicBool::new(false);
    let mut reader = CancellingReader(&cancelled, 0);
    let mut output = Vec::new();
    assert!(copy(&mut reader, &mut output, &cancelled).is_err());
    assert_eq!(reader.1, 1);
    assert_eq!(output, [42]);
}

#[test]
fn names_never_carry_path_or_drive_syntax() {
    for bad in ["", ".", "..", "a/b", "a\\b", "C:evil.bat", "line\nbreak"] {
        assert!(name(bad).is_err(), "{bad:?}");
    }
    assert_eq!(name(".hidden").unwrap(), ".hidden");
}

#[test]
fn snapshot_all_keeps_order_and_a_bad_path_leaves_nothing() {
    let temp = Temp::new();
    let root = temp.0.join("snapshots");
    for n in ["a", "b", "c"] {
        std::fs::write(temp.0.join(n), n).unwrap();
    }
    let paths = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let cancelled = AtomicBool::new(false);
    let saved = snapshot_all(
        &root,
        &temp.0,
        &paths(&["c", "a", "b", "a"]),
        None,
        &Reach::new([temp.0.clone()], Roots::default()),
        &cancelled,
    )
    .unwrap();
    let names: Vec<_> = saved.iter().map(|s| s.meta.name.as_str()).collect();
    assert_eq!(names, ["c", "a", "b", "a"]);
    drop(saved);
    let error = snapshot_all(
        &root,
        &temp.0,
        &paths(&["a", "missing", "b"]),
        None,
        &Reach::new([temp.0.clone()], Roots::default()),
        &cancelled,
    )
    .err()
    .unwrap();
    assert!(format!("{error:#}").contains("missing"));
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}

/// N1: `send_file` never copies a secret into a chat, nor anything outside the run's folders, however it is spelled.
#[cfg(unix)]
#[test]
fn send_file_reaches_only_the_runs_folders_and_never_a_secret() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let base = temp.0.canonicalize().unwrap();
    let (kith, work, outside) = (base.join("kith"), base.join("work"), base.join("outside"));
    for dir in
        [kith.join("secrets"), kith.join("serve"), kith.join("agents/coder/files"), work.clone(), outside.clone()]
    {
        std::fs::create_dir_all(dir).unwrap();
    }
    for file in [
        kith.join("secrets/auth.json"),
        kith.join("serve/token"),
        kith.join("agents/coder/files/render.png"),
        work.join("out.png"),
        outside.join("notes.txt"),
    ] {
        std::fs::write(file, "x").unwrap();
    }
    symlink(kith.join("secrets/auth.json"), work.join("auth-link")).unwrap();
    symlink(outside.join("notes.txt"), work.join("notes-link")).unwrap();
    // An owner run: its working folder, and the Kith folder its launch adds (which holds the agent's files folder).
    let reach = Reach::new([work.clone(), kith.clone()], Roots::new(vec![kith.clone()]));
    let root = base.join("snapshots");
    let share = |source: &str| snapshot_all(&root, &work, &[source.to_owned()], None, &reach, &AtomicBool::new(false));
    let refused = |source: &str, why: &str| {
        let error = format!("{:#}", share(source).err().unwrap_or_else(|| panic!("{source} was shared")));
        assert!(error.contains(why), "{source}: {error}");
    };
    let secret = kith.join("secrets/auth.json").to_string_lossy().into_owned();
    refused(&secret, "secrets list");
    refused("../kith/secrets/auth.json", "secrets list");
    refused("../kith/serve/token", "secrets list");
    refused("auth-link", "secrets list");
    refused(&outside.join("notes.txt").to_string_lossy(), "outside this run's folders");
    refused("../outside/notes.txt", "outside this run's folders");
    refused("notes-link", "outside this run's folders");
    refused("/etc/hosts", "outside this run's folders");
    assert_eq!(std::fs::read_dir(&root).map_or(0, Iterator::count), 0, "a refused share leaves no copy");
    let sent = share("out.png").unwrap();
    assert_eq!(sent[0].meta.name, "out.png");
    let render = kith.join("agents/coder/files/render.png").to_string_lossy().into_owned();
    assert_eq!(share(&render).unwrap()[0].meta.name, "render.png");
}

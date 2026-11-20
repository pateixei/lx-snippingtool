use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use lx_snip_core::storage::SnipStore;
use uuid::Uuid;

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{prefix}_{}", Uuid::new_v4()));
        Self(dir)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn empty_nonexistent_store_lists_empty() {
    let guard = TempDirGuard::new("lx_snip_store_empty");
    let store = SnipStore::new(&guard.0, 50);
    assert!(store.list().unwrap().is_empty());
    assert_eq!(store.prune().unwrap(), 0);
}

#[test]
fn enforces_0700_dir_and_0600_file_permissions() {
    let guard = TempDirGuard::new("lx_snip_store_perms");
    let store = SnipStore::new(&guard.0, 10);

    let saved = store.save_png(b"\x89PNG\r\n\x1a\nfake").unwrap();
    assert!(saved.exists());

    #[cfg(unix)]
    {
        let dir_mode = fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "expected 0700 on snips dir, got {dir_mode:o}"
        );

        let file_mode = fs::metadata(&saved).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            file_mode, 0o600,
            "expected 0600 on snip file, got {file_mode:o}"
        );
    }
}

#[test]
fn lists_newest_first_and_prunes_oldest_by_mtime() {
    let guard = TempDirGuard::new("lx_snip_store_prune");
    let store = SnipStore::new(&guard.0, 2);
    store.ensure_dir().unwrap();

    let base_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let p1 = store.dir().join("1_old.png");
    let p2 = store.dir().join("2_mid.png");
    let p3 = store.dir().join("3_new.png");
    let non_png = store.dir().join("notes.txt");

    fs::write(&p1, b"old").unwrap();
    fs::write(&p2, b"mid").unwrap();
    fs::write(&p3, b"new").unwrap();
    fs::write(&non_png, b"ignore").unwrap();

    fs::File::options()
        .write(true)
        .open(&p1)
        .unwrap()
        .set_modified(base_time)
        .unwrap();
    fs::File::options()
        .write(true)
        .open(&p2)
        .unwrap()
        .set_modified(base_time + Duration::from_secs(10))
        .unwrap();
    fs::File::options()
        .write(true)
        .open(&p3)
        .unwrap()
        .set_modified(base_time + Duration::from_secs(20))
        .unwrap();

    let before_prune = store.list().unwrap();
    assert_eq!(before_prune.len(), 3);
    assert_eq!(before_prune[0].path, p3);
    assert_eq!(before_prune[1].path, p2);
    assert_eq!(before_prune[2].path, p1);

    let removed = store.prune().unwrap();
    assert_eq!(removed, 1);

    let after_prune = store.list().unwrap();
    assert_eq!(after_prune.len(), 2);
    assert_eq!(after_prune[0].path, p3);
    assert_eq!(after_prune[1].path, p2);
    assert!(!p1.exists());
    assert!(non_png.exists());

    store.delete(&p2).unwrap();
    assert_eq!(store.list().unwrap().len(), 1);
}

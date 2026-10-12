//! Day 24 integration tests for local backup.
//!
//! These use a fake `mysqldump` on $PATH (a shell script) so the test
//! does not need a live MySQL server. The tar/zstd path is exercised
//! end-to-end against a temp dir.
//!
//! Serialised on ENV_LOCK because both tests touch $PATH.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use vajra::backup::local;
use vajra::config::{BackupConfig, BackupDatabase, BackupDestination, BackupFiles};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vajra-backup-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A fake `mysqldump` that ignores its args and writes deterministic SQL.
fn fake_mysqldump(dir: &std::path::Path, out_sql: &str) -> PathBuf {
    let bin = dir.join("mysqldump");
    let script = format!("#!/bin/sh\ncat <<'SQL'\n{out_sql}\nSQL\n");
    fs::write(&bin, script).unwrap();
    let mut p = fs::metadata(&bin).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&bin, p).unwrap();
    bin
}

fn with_path_prefix<R>(prefix: &std::path::Path, f: impl FnOnce() -> R) -> R {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut parts: Vec<PathBuf> = vec![prefix.to_path_buf()];
    parts.extend(std::env::split_paths(&old));
    let new = std::env::join_paths(parts).unwrap();
    std::env::set_var("PATH", &new);
    let r = f();
    std::env::set_var("PATH", &old);
    r
}

#[test]
fn local_backup_writes_tar_zst_and_manifest() {
    let tmp = tmpdir("local-ok");
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fake_mysqldump(&bin, "CREATE TABLE t (id INT);\n");

    let site = tmp.join("site");
    let wpc = site.join("wp-content");
    fs::create_dir_all(&wpc).unwrap();
    fs::write(wpc.join("index.php"), "<?php echo 'hi';").unwrap();
    fs::write(wpc.join("style.css"), "body{color:red}").unwrap();

    let local_dir = tmp.join("backups");

    let cfg = BackupConfig {
        enabled: true,
        destinations: vec![BackupDestination::Local],
        local_dir: local_dir.clone(),
        keep_days: 7,
        database: Some(BackupDatabase {
            host: "localhost".into(),
            port: 3306,
            user: "u".into(),
            password: None,
            name: "wp".into(),
        }),
        files: Some(BackupFiles {
            include: vec![wpc.clone()],
        }),
        ..BackupConfig::default()
    };

    let report = with_path_prefix(&bin, || local::run(&cfg).expect("backup ok"));
    assert!(report.archive_bytes > 0);
    assert_eq!(report.pruned_local, 0);

    let path = report.local_path.expect("local path");
    assert!(path.exists(), "archive missing: {path:?}");
    assert!(path.to_string_lossy().ends_with(".tar.zst"));

    // Extract and verify contents.
    let f = fs::File::open(&path).unwrap();
    let dec = zstd::Decoder::new(f).unwrap();
    let mut ar = tar::Archive::new(dec);
    let mut seen_db = false;
    let mut seen_css = false;
    let mut seen_manifest = false;
    for entry in ar.entries().unwrap() {
        let e = entry.unwrap();
        let p = e.path().unwrap().to_string_lossy().into_owned();
        if p.ends_with("db.sql") {
            seen_db = true;
        }
        if p.ends_with("style.css") {
            seen_css = true;
        }
        if p.ends_with("MANIFEST") {
            seen_manifest = true;
        }
    }
    assert!(seen_db, "db.sql not in archive");
    assert!(seen_css, "style.css not in archive");
    assert!(seen_manifest, "MANIFEST not in archive");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn local_backup_prunes_old_archives() {
    let tmp = tmpdir("local-prune");
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fake_mysqldump(&bin, "-- sql\n");

    let site = tmp.join("site");
    let wpc = site.join("wp-content");
    fs::create_dir_all(&wpc).unwrap();
    fs::write(wpc.join("a.txt"), "a").unwrap();

    let local_dir = tmp.join("backups");
    fs::create_dir_all(&local_dir).unwrap();

    // Seed two "old" archives (mtime 30 days ago).
    let old1 = local_dir.join("vajra-2000-01-01T00-00-00Z.tar.zst");
    let old2 = local_dir.join("vajra-2000-01-02T00-00-00Z.tar.zst");
    fs::write(&old1, b"old").unwrap();
    fs::write(&old2, b"old").unwrap();
    let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86400);
    let ft = std::fs::FileTimes::new().set_modified(old_time);
    fs::File::open(&old1).unwrap().set_times(ft).unwrap();
    fs::File::open(&old2).unwrap().set_times(ft).unwrap();

    let cfg = BackupConfig {
        enabled: true,
        destinations: vec![BackupDestination::Local],
        local_dir: local_dir.clone(),
        keep_days: 7,
        database: None,
        files: Some(BackupFiles {
            include: vec![wpc.clone()],
        }),
        ..BackupConfig::default()
    };

    let report = with_path_prefix(&bin, || local::run(&cfg).expect("backup ok"));
    assert_eq!(
        report.pruned_local, 2,
        "expected 2 pruned, got {}",
        report.pruned_local
    );
    assert!(!old1.exists(), "old1 should be gone");
    assert!(!old2.exists(), "old2 should be gone");

    // At least the new archive must exist.
    let now_archives: Vec<_> = fs::read_dir(&local_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("vajra-"))
        .collect();
    assert!(!now_archives.is_empty(), "fresh archive missing");

    let _ = fs::remove_dir_all(&tmp);
}

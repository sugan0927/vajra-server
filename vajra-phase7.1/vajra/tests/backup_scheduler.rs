//! Day 26 integration: the background scheduler actually fires.
//!
//! We use a `* * * * * *` schedule (fires every second) and
//! `VAJRA_BACKUP_TICK_SECS=1` so the test completes in ~3–4 seconds.
//! ENV_LOCK serialises against other env-touching tests in this binary.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use vajra::config::{BackupConfig, BackupDestination, BackupFiles};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vajra-sched-{tag}-{}-{}",
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

fn count_archives(dir: &std::path::Path) -> usize {
    fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("vajra-"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn scheduler_runs_backup_on_secondly_schedule() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::set_var("VAJRA_BACKUP_TICK_SECS", "1");

    let tmp = tmpdir("secondly");
    let local_dir = tmp.join("backups");
    let site = tmp.join("site");
    let wpc = site.join("wp-content");
    fs::create_dir_all(&wpc).unwrap();
    fs::write(wpc.join("index.php"), "<?php echo 'hi';").unwrap();

    let cfg = BackupConfig {
        enabled: true,
        destinations: vec![BackupDestination::Local],
        local_dir: local_dir.clone(),
        keep_days: 1,
        schedule: "* * * * * *".into(), // every second
        database: None,
        files: Some(BackupFiles {
            include: vec![wpc.clone()],
        }),
        ..BackupConfig::default()
    };

    let mut sched = vajra::backup::scheduler::spawn(cfg);

    // Wait up to 6 s for the first archive to appear. The scheduler
    // only checks once per tick (~1 s), so 3–4 s is usually enough.
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    loop {
        if count_archives(&local_dir) >= 1 {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "no archive after 6 s; local_dir={:?}, contents={:?}",
                local_dir,
                fs::read_dir(&local_dir)
                    .map(|it| it
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name())
                        .collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    sched.shutdown();
    assert!(count_archives(&local_dir) >= 1);

    std::env::remove_var("VAJRA_BACKUP_TICK_SECS");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn disabled_scheduler_does_not_create_files() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::set_var("VAJRA_BACKUP_TICK_SECS", "1");

    let tmp = tmpdir("disabled");
    let local_dir = tmp.join("backups");
    let cfg = BackupConfig {
        enabled: false,
        destinations: vec![BackupDestination::Local],
        local_dir: local_dir.clone(),
        schedule: "* * * * * *".into(),
        ..BackupConfig::default()
    };

    let mut sched = vajra::backup::scheduler::spawn(cfg);
    std::thread::sleep(Duration::from_secs(2));
    sched.shutdown();

    // No thread should have run, and local_dir should not exist (the
    // local path would create_dir_all it as its first action).
    assert!(
        !local_dir.exists(),
        "disabled scheduler must not touch local_dir"
    );

    std::env::remove_var("VAJRA_BACKUP_TICK_SECS");
    let _ = fs::remove_dir_all(&tmp);
}

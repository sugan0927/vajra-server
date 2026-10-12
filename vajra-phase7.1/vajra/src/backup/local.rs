//! Local backup: mysqldump + tar of wp-content → `<local_dir>/vajra-*.tar.zst`.
//!
//! Layout inside the archive:
//! ```text
//! vajra-backup-YYYY-MM-DDTHH-MM-SSZ/
//!   db.sql                 # mysqldump output (if db section enabled)
//!   wp-content/            # verbatim copy of the site tree
//!   MANIFEST               # small text file: version, host, date, sizes
//! ```

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{BackupConfig, BackupDatabase, BackupFiles};

use super::{BackupError, BackupReport};

/// Run one local backup: dump DB (if enabled), tar wp-content, write
/// `<local_dir>/vajra-<timestamp>.tar.zst`, prune old archives.
pub fn run(cfg: &BackupConfig) -> Result<BackupReport, BackupError> {
    if !cfg
        .destinations
        .contains(&crate::config::BackupDestination::Local)
    {
        return Ok(BackupReport {
            local_path: None,
            archive_bytes: 0,
            pruned_local: 0,
            s3_key: None,
            pruned_s3: 0,
        });
    }

    fs::create_dir_all(&cfg.local_dir)?;

    // ── 1. Temp staging dir (same filesystem as local_dir, so rename is atomic).
    let staging = tempfile::Builder::new()
        .prefix(".vajra-backup-")
        .tempdir_in(&cfg.local_dir)?;
    let stage_root = staging.path().join("vajra-backup");

    fs::create_dir_all(&stage_root)?;

    // ── 2. mysqldump (if configured).
    let db_bytes = if let Some(db) = &cfg.database {
        dump_db(db, &stage_root.join("db.sql"))?
    } else {
        0
    };

    // ── 3. tar wp-content (or a custom files list).
    let files_bytes = if let Some(files) = &cfg.files {
        tar_files(files, &stage_root)?
    } else {
        0
    };

    // ── 4. MANIFEST.
    write_manifest(&stage_root, db_bytes, files_bytes)?;

    // ── 5. tar.zst the staging dir into a `.partial` file, then rename.
    let ts = timestamp_compact();
    let final_name = format!("vajra-{ts}.tar.zst");
    let final_path = cfg.local_dir.join(&final_name);
    let partial_path = cfg.local_dir.join(format!(".{final_name}.partial"));

    let archive_bytes = tar_zstd(&stage_root, &partial_path)?;
    fs::rename(&partial_path, &final_path)?;

    // ── 6. Prune old archives.
    let pruned = prune_old(&cfg.local_dir, cfg.keep_days)?;

    Ok(BackupReport {
        local_path: Some(final_path),
        archive_bytes,
        pruned_local: pruned,
        s3_key: None,
        pruned_s3: 0,
    })
}

// ─────────────────────────────────────────────────────────────────────

/// `mysqldump --single-transaction --quick ...` — the safe default for
/// InnoDB: consistent snapshot, no long locks, streams rows.
fn dump_db(db: &BackupDatabase, out: &Path) -> Result<u64, BackupError> {
    let mut cmd = Command::new("mysqldump");
    cmd.arg("--single-transaction")
        .arg("--quick")
        .arg("--skip-lock-tables")
        .arg("--default-character-set=utf8mb4")
        .arg("--hex-blob")
        .arg("-h")
        .arg(&db.host)
        .arg("-P")
        .arg(db.port.to_string())
        .arg("-u")
        .arg(&db.user);

    // Password: prefer `MYSQL_PWD` env (avoids exposing it on the
    // process command line, which `ps` would reveal).
    if let Some(pw) = &db.password {
        cmd.env("MYSQL_PWD", pw);
    }

    cmd.arg(&db.name);

    let out_file = File::create(out)?;
    cmd.stdout(out_file);

    let status = cmd.status()?;
    if !status.success() {
        return Err(BackupError::Mysqldump(format!(
            "mysqldump exited with {status}"
        )));
    }

    Ok(fs::metadata(out)?.len())
}

/// `tar::Builder::append_dir_all` under a stable prefix.
/// See docs: builder.append_dir_all("vajra-backup/wp-content", src)
/// preserves the tree under the given name. [reference:1]
fn tar_files(files: &BackupFiles, stage_root: &Path) -> Result<u64, BackupError> {
    let mut total = 0u64;
    for entry in &files.include {
        let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("data");
        let dst = stage_root.join(name);

        if entry.is_dir() {
            copy_dir_recursive(entry, &dst)?;
            total += dir_size(&dst)?;
        } else {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry, &dst)?;
            total += fs::metadata(&dst)?.len();
        }
    }
    Ok(total)
}

/// Stream `src_root` into `out` as a zstd-compressed tar.
fn tar_zstd(src_root: &Path, out: &Path) -> Result<u64, BackupError> {
    let f = File::create(out)?;
    // Level 3 is a good speed/ratio default for site backups.
    let enc = zstd::Encoder::new(f, 3)?;
    let mut builder = tar::Builder::new(enc);
    builder.follow_symlinks(false);

    // Append the whole staging dir under the name "vajra-backup".
    builder.append_dir_all("vajra-backup", src_root)?;
    let enc = builder.into_inner()?;
    enc.finish()?;

    Ok(fs::metadata(out)?.len())
}

fn write_manifest(stage_root: &Path, db_bytes: u64, files_bytes: u64) -> Result<(), BackupError> {
    let host = std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string();
    let mut f = File::create(stage_root.join("MANIFEST"))?;
    writeln!(f, "vajra-backup v1")?;
    writeln!(f, "host: {host}")?;
    writeln!(f, "timestamp: {}", timestamp_iso())?;
    writeln!(f, "db_bytes: {db_bytes}")?;
    writeln!(f, "files_bytes: {files_bytes}")?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Retention
// ─────────────────────────────────────────────────────────────────────

/// Delete `vajra-*.tar.zst` files whose mtime is older than `keep_days`.
/// Partial files (`.vajra-*.partial`) older than 1 day are also removed.
fn prune_old(dir: &Path, keep_days: u32) -> Result<usize, BackupError> {
    let cutoff = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(keep_days as u64 * 86400))
        .unwrap_or(UNIX_EPOCH);

    let mut removed = 0usize;
    for ent in fs::read_dir(dir)? {
        let ent = ent?;
        let name = match ent.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let is_archive = name.starts_with("vajra-") && name.ends_with(".tar.zst");
        let is_partial = name.starts_with(".vajra-") && name.ends_with(".partial");
        if !is_archive && !is_partial {
            continue;
        }
        let md = ent.metadata()?;
        if md.modified()? < cutoff {
            fs::remove_file(ent.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

// ─────────────────────────────────────────────────────────────────────
// Small helpers
// ─────────────────────────────────────────────────────────────────────

fn timestamp_compact() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // YYYY-MM-DDTHH-MM-SSZ (sortable, no colons → filesystem-safe).
    let secs = now.as_secs();
    let days = secs / 86400;
    let rem = secs % 86400;
    let (y, m, d) = civil_from_days(days as i64);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}-{mm:02}-{ss:02}Z")
}

fn timestamp_iso() -> String {
    timestamp_compact()
}

/// Howard Hinnant's `civil_from_days` — days since 1970-01-01 → Y/M/D.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + (m <= 2) as i64) as i32, m, d)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), BackupError> {
    fs::create_dir_all(dst)?;
    for ent in fs::read_dir(src)? {
        let ent = ent?;
        let ft = ent.file_type()?;
        let to = dst.join(ent.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&ent.path(), &to)?;
        } else if ft.is_file() {
            fs::copy(ent.path(), &to)?;
        }
        // Symlinks intentionally skipped — see docs/backup.md non-goals.
    }
    Ok(())
}

fn dir_size(dir: &Path) -> Result<u64, BackupError> {
    let mut total = 0u64;
    for ent in fs::read_dir(dir)? {
        let ent = ent?;
        let md = ent.metadata()?;
        if md.is_dir() {
            total += dir_size(&ent.path())?;
        } else {
            total += md.len();
        }
    }
    Ok(total)
}

// Silence unused import when only files (not db) is configured.
#[allow(dead_code)]
fn _read_marker<R: Read>(_: &mut R) {}

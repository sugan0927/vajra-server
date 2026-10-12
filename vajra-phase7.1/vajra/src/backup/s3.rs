//! S3 backup destination — uploads the local archive written by
//! `backup::local::run`.
//!
//! Uses `rust-s3`'s blocking client (`Bucket::put_object_blocking`),
//! which wraps the async path internally without pulling tokio into the
//! rest of Vajra. See docs/backup.md §S3 for credential resolution.
//!
//! Multipart: `put_object_blocking` automatically switches to multipart
//! for objects above S3's single-PUT threshold when the `multipart`
//! feature is enabled. We enable it in Cargo.toml.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use s3::creds::Credentials;
use s3::{Bucket, Region};

use crate::config::BackupConfig;

use super::{BackupError, BackupReport};

/// Upload one archive to the configured bucket.
///
/// `local_path` must be a file that already exists (produced by
/// `backup::local::run`). Returns the S3 key that was written.
pub fn upload(cfg: &BackupConfig, local_path: &Path) -> Result<String, BackupError> {
    let bucket_name = cfg
        .s3_bucket
        .as_deref()
        .ok_or_else(|| BackupError::Config("s3_bucket is required".into()))?;
    let region_name = cfg
        .s3_region
        .as_deref()
        .ok_or_else(|| BackupError::Config("s3_region is required".into()))?;

    let (ak, sk) = cfg.resolved_s3_creds();
    let creds = match (ak, sk) {
        (Some(a), Some(s)) => Credentials::new(Some(&a), Some(&s), None, None, None)
            .map_err(|e| BackupError::S3(format!("credentials: {e}")))?,
        // Fall back to env / ~/.aws/credentials / instance role.
        _ => Credentials::default()
            .map_err(|e| BackupError::S3(format!("credentials (default): {e}")))?,
    };

    let region = if let Some(endpoint) = &cfg.s3_endpoint {
        // MinIO / R2 / B2: path-style + explicit endpoint.
        Region::Custom {
            region: region_name.to_string(),
            endpoint: endpoint.clone(),
        }
    } else {
        region_name
            .parse::<Region>()
            .map_err(|e| BackupError::S3(format!("region {region_name:?}: {e}")))?
    };

    let bucket = Bucket::new(bucket_name, region, creds)
        .map_err(|e| BackupError::S3(format!("bucket {bucket_name:?}: {e}")))?;

    // Path-style when a custom endpoint is set (MinIO et al.).
    let bucket = if cfg.s3_endpoint.is_some() {
        bucket.with_path_style()
    } else {
        bucket
    };

    let key = format!(
        "{}{}",
        cfg.s3_prefix,
        local_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| BackupError::S3("archive has no filename".into()))?
    );

    let mut f = File::open(local_path)?;
    let mut body = Vec::new();
    f.read_to_end(&mut body)?;

    let content_type = "application/zstd";

    // put_object_blocking picks multipart above the crate's threshold
    // when the `multipart` feature is on.
    let resp = bucket
        .put_object_blocking(&key, &body)
        .map_err(|e| BackupError::S3(format!("put_object_blocking: {e}")))?;

    if resp.status_code() / 100 != 2 {
        return Err(BackupError::S3(format!(
            "unexpected status {} for {key}",
            resp.status_code()
        )));
    }
    let _ = content_type; // TODO(Day 26): set Content-Type header via put_object_with_content_type.

    Ok(key)
}

/// Prune old objects in the bucket whose key starts with `s3_prefix`
/// and whose `Last-Modified` is older than `keep_days`.
///
/// Returns the number of objects deleted. Errors are non-fatal —
/// a failed prune does not fail the whole backup run.
pub fn prune_old(cfg: &BackupConfig) -> Result<usize, BackupError> {
    let bucket_name = match cfg.s3_bucket.as_deref() {
        Some(b) => b,
        None => return Ok(0),
    };
    let region_name = cfg.s3_region.as_deref().unwrap_or("us-east-1");
    let (ak, sk) = cfg.resolved_s3_creds();
    let creds = match (ak, sk) {
        (Some(a), Some(s)) => Credentials::new(Some(&a), Some(&s), None, None, None)
            .map_err(|e| BackupError::S3(format!("credentials: {e}")))?,
        _ => Credentials::default()
            .map_err(|e| BackupError::S3(format!("credentials (default): {e}")))?,
    };
    let region = if let Some(endpoint) = &cfg.s3_endpoint {
        Region::Custom {
            region: region_name.to_string(),
            endpoint: endpoint.clone(),
        }
    } else {
        region_name
            .parse::<Region>()
            .map_err(|e| BackupError::S3(format!("region: {e}")))?
    };
    let bucket = Bucket::new(bucket_name, region, creds)
        .map_err(|e| BackupError::S3(format!("bucket: {e}")))?;
    let bucket = if cfg.s3_endpoint.is_some() {
        bucket.with_path_style()
    } else {
        bucket
    };

    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(cfg.keep_days as u64 * 86400))
        .unwrap_or(std::time::UNIX_EPOCH);

    let mut removed = 0usize;
    let (list, _code) = bucket
        .list(cfg.s3_prefix.clone(), Some("/".to_string()))
        .map_err(|e| BackupError::S3(format!("list: {e}")))?;
    for obj in list {
        let Some(last_modified) = obj.last_modified.clone() else {
            continue;
        };
        if parse_http_date(&last_modified) < Some(cutoff) {
            if bucket.delete_object_blocking(&obj.key).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Very small HTTP-date parser (RFC 7231 IMF-fixdate). Returns `None`
/// on any unexpected format — pruning just skips that object then.
/// Test-only re-export so integration tests can exercise the
/// HTTP-date parser without constructing a live S3 client.
pub fn parse_http_date_for_test(s: &str) -> Option<std::time::SystemTime> {
    parse_http_date(s)
}

fn parse_http_date(s: &str) -> Option<std::time::SystemTime> {
    // Format: "Sun, 06 Nov 1994 08:49:37 GMT"
    let bytes = s.as_bytes();
    if bytes.len() < 29 {
        return None;
    }
    let day: u32 = std::str::from_utf8(&bytes[5..7]).ok()?.parse().ok()?;
    let month = match &bytes[8..11] {
        b"Jan" => 1u32,
        b"Feb" => 2,
        b"Mar" => 3,
        b"Apr" => 4,
        b"May" => 5,
        b"Jun" => 6,
        b"Jul" => 7,
        b"Aug" => 8,
        b"Sep" => 9,
        b"Oct" => 10,
        b"Nov" => 11,
        b"Dec" => 12,
        _ => return None,
    };
    let year: i32 = std::str::from_utf8(&bytes[12..16]).ok()?.parse().ok()?;
    let hour: u32 = std::str::from_utf8(&bytes[17..19]).ok()?.parse().ok()?;
    let min: u32 = std::str::from_utf8(&bytes[20..22]).ok()?.parse().ok()?;
    let sec: u32 = std::str::from_utf8(&bytes[23..25]).ok()?.parse().ok()?;

    let days = days_from_civil(year, month, day)?;
    let secs = days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    if secs < 0 {
        return None;
    }
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
}

fn days_from_civil(y: i32, m: u32, d: u32) -> Option<i64> {
    let y = y as i64 - (m <= 2) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// Compose the two halves: local archive + S3 upload + prune.
pub fn run(cfg: &BackupConfig) -> Result<BackupReport, BackupError> {
    // The local step must run first — S3 uploads the file it produced.
    let local = super::local::run(cfg)?;
    let path = match &local.local_path {
        Some(p) => p.clone(),
        None => {
            return Err(BackupError::Config(
                "s3 destination configured but no local archive was produced".into(),
            ))
        }
    };
    let key = upload(cfg, &path)?;
    let pruned = prune_old(cfg).unwrap_or(0);
    Ok(BackupReport {
        s3_key: Some(key),
        pruned_s3: pruned,
        ..local
    })
}

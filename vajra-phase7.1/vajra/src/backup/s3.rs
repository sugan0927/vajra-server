//! S3 backup destination — uploads the archive written by
//! `backup::local::run`.
//!
//! Uses `rust-s3`'s blocking client (`put_object_blocking`), which
//! wraps the async path internally without pulling tokio into the rest
//! of Vajra.
//!
//! Retention: local-only for now. `backup::local::prune_old` handles
//! both archives and orphaned `.partial` files; S3-side retention is a
//! follow-up. The Day 25 task only required upload (single PUT +
//! multipart above the crate threshold).
//!
//! See `docs/backup.md` §S3 for credential resolution.

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

    // put_object_blocking picks multipart above the crate's threshold.
    let resp = bucket
        .put_object_blocking(&key, &body)
        .map_err(|e| BackupError::S3(format!("put_object_blocking: {e}")))?;

    if resp.status_code() / 100 != 2 {
        return Err(BackupError::S3(format!(
            "unexpected status {} for {key}",
            resp.status_code()
        )));
    }

    Ok(key)
}

/// Compose the two halves: local archive + S3 upload.
///
/// Local retention still runs (via `backup::local::run`). S3-side
/// retention is deferred — see the module docstring.
pub fn run(cfg: &BackupConfig) -> Result<BackupReport, BackupError> {
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
    Ok(BackupReport {
        s3_key: Some(key),
        pruned_s3: 0,
        ..local
    })
}

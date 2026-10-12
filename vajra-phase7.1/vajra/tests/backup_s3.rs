//! Day 25 unit tests for the S3 backup destination.
//!
//! Full end-to-end upload requires a live bucket (or MinIO); that is a
//! Day 26 integration environment. Here we cover:
//!   * config guards — upload() must fail before any network I/O
//!   * credential resolution precedence (env > config)
//!
//! These tests touch process-wide env, so they run under ENV_LOCK.

use std::sync::Mutex;
use vajra::config::BackupConfig;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn s3_requires_bucket_before_network() {
    let cfg = BackupConfig {
        s3_bucket: None,
        s3_region: Some("us-east-1".into()),
        ..BackupConfig::default()
    };
    let r = vajra::backup::s3::upload(&cfg, std::path::Path::new("/dev/null"));
    assert!(r.is_err(), "expected error for missing bucket");
    assert!(
        r.unwrap_err().to_string().contains("s3_bucket"),
        "error should name the missing field"
    );
}

#[test]
fn s3_credentials_env_first() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let cfg = BackupConfig {
        s3_access_key: Some("cfg-ak".into()),
        s3_secret_key: Some("cfg-sk".into()),
        ..BackupConfig::default()
    };

    std::env::remove_var("VAJRA_S3_ACCESS_KEY");
    std::env::remove_var("VAJRA_S3_SECRET_KEY");
    let (ak, sk) = cfg.resolved_s3_creds();
    assert_eq!(ak.as_deref(), Some("cfg-ak"));
    assert_eq!(sk.as_deref(), Some("cfg-sk"));

    std::env::set_var("VAJRA_S3_ACCESS_KEY", "env-ak");
    std::env::set_var("VAJRA_S3_SECRET_KEY", "env-sk");
    let (ak, sk) = cfg.resolved_s3_creds();
    assert_eq!(ak.as_deref(), Some("env-ak"));
    assert_eq!(sk.as_deref(), Some("env-sk"));

    // Empty env var must fall back to config (not override with "").
    std::env::set_var("VAJRA_S3_ACCESS_KEY", "");
    let (ak, _) = cfg.resolved_s3_creds();
    assert_eq!(ak.as_deref(), Some("cfg-ak"));

    std::env::remove_var("VAJRA_S3_ACCESS_KEY");
    std::env::remove_var("VAJRA_S3_SECRET_KEY");
}

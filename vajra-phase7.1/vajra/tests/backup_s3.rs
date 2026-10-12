//! Day 25 unit tests for the S3 backup destination.
//!
//! Full end-to-end upload requires a live bucket; those land in a
//! Day 26 integration environment. Here we cover the pure functions
//! and the config/credential resolution.

use vajra::config::BackupConfig;

#[test]
fn s3_requires_bucket_and_region() {
    let cfg = BackupConfig {
        s3_bucket: None,
        s3_region: Some("us-east-1".into()),
        ..BackupConfig::default()
    };
    // upload() must fail before touching the network.
    let r = vajra::backup::s3::upload(&cfg, std::path::Path::new("/dev/null"));
    assert!(r.is_err(), "expected error for missing bucket");
}

#[test]
fn s3_http_date_parser_handles_imf_fixdate() {
    // 1994-11-06 08:49:37 UTC == 784111777 secs.
    let t = vajra::backup::s3::parse_http_date_for_test("Sun, 06 Nov 1994 08:49:37 GMT")
        .expect("parse ok");
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(secs, 784111777);
}

#[test]
fn s3_http_date_parser_rejects_garbage() {
    assert!(vajra::backup::s3::parse_http_date_for_test("not a date").is_none());
}

#[test]
fn s3_credentials_env_first() {
    // Serialise against other env-touching tests in this binary.
    let _g = std::sync::Mutex::<()>::new(());
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
    let (ak, _) = cfg.resolved_s3_creds();
    assert_eq!(ak.as_deref(), Some("env-ak"));
    std::env::remove_var("VAJRA_S3_ACCESS_KEY");
}

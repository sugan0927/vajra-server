//! Parse-layer tests for the `[backup]` config section (Day 23).

use std::sync::Mutex;
use vajra::config::BackupConfig;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn parse(src: &str) -> Result<BackupConfig, toml::de::Error> {
    toml::from_str::<BackupConfig>(src)
}

#[test]
fn empty_section_uses_defaults() {
    let c = parse("").expect("empty section must parse");
    assert!(!c.enabled);
    assert_eq!(c.schedule, "0 3 * * *");
    assert_eq!(c.keep_days, 7);
    assert!(c.destinations.is_empty());
    assert_eq!(c.s3_prefix, "vajra/");
}

#[test]
fn rejects_unknown_field() {
    let src = "enabled = true\ns3_bucket_name = \"oops\"\n";
    let err = parse(src).expect_err("typo must fail");
    assert!(err.to_string().contains("s3_bucket_name"), "got: {err}");
}

#[test]
fn rejects_enabled_with_empty_destinations() {
    let c = parse("enabled = true\n").expect("parse ok");
    assert!(c.validate().is_err());
}

#[test]
fn rejects_bad_cron() {
    let c = parse("enabled = true\ndestinations = [\"local\"]\nschedule = \"not a cron\"\n")
        .expect("parse ok");
    assert!(c.validate().is_err());
}

#[test]
fn rejects_keep_days_zero() {
    let c = parse("enabled = true\ndestinations = [\"local\"]\nkeep_days = 0\n").expect("parse ok");
    assert!(c.validate().is_err());
}

#[test]
fn rejects_keep_days_too_large() {
    let c =
        parse("enabled = true\ndestinations = [\"local\"]\nkeep_days = 4000\n").expect("parse ok");
    assert!(c.validate().is_err());
}

#[test]
fn s3_without_bucket_fails() {
    let c = parse("enabled = true\ndestinations = [\"s3\"]\ns3_region = \"us-east-1\"\n")
        .expect("parse ok");
    assert!(c.validate().is_err());
}

#[test]
fn s3_without_region_fails() {
    let c =
        parse("enabled = true\ndestinations = [\"s3\"]\ns3_bucket = \"b\"\n").expect("parse ok");
    assert!(c.validate().is_err());
}

#[test]
fn local_dir_must_be_absolute() {
    let c = parse("enabled = true\ndestinations = [\"local\"]\nlocal_dir = \"relative/path\"\n")
        .expect("parse ok");
    assert!(c.validate().is_err());
}

#[test]
fn unknown_destination_fails() {
    let src = "enabled = true\ndestinations = [\"gcs\"]\n";
    assert!(parse(src).is_err(), "unknown destination must fail parse");
}

#[test]
fn env_overrides_config_secret() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let src = "enabled = true\ndestinations = [\"s3\"]\n\
               s3_bucket = \"b\"\ns3_region = \"us-east-1\"\n\
               s3_access_key = \"from-config\"\ns3_secret_key = \"from-config-secret\"\n";
    let c = parse(src).expect("parse ok");

    std::env::remove_var("VAJRA_S3_ACCESS_KEY");
    std::env::remove_var("VAJRA_S3_SECRET_KEY");
    let (ak, sk) = c.resolved_s3_creds();
    assert_eq!(ak.as_deref(), Some("from-config"));
    assert_eq!(sk.as_deref(), Some("from-config-secret"));

    std::env::set_var("VAJRA_S3_ACCESS_KEY", "from-env");
    std::env::set_var("VAJRA_S3_SECRET_KEY", "from-env-secret");
    let (ak, sk) = c.resolved_s3_creds();
    assert_eq!(ak.as_deref(), Some("from-env"));
    assert_eq!(sk.as_deref(), Some("from-env-secret"));

    std::env::set_var("VAJRA_S3_ACCESS_KEY", "");
    let (ak, _) = c.resolved_s3_creds();
    assert_eq!(ak.as_deref(), Some("from-config"));

    std::env::remove_var("VAJRA_S3_ACCESS_KEY");
    std::env::remove_var("VAJRA_S3_SECRET_KEY");
}

/// Regression: `BackupConfig::default().schedule` must itself parse.
/// A previous default used a 5-field expression (`"0 3 * * *"`) that the
/// `cron` crate rejects — so `enabled = true` with a minimal config
/// would fail load. This test would have caught it.
#[test]
fn default_schedule_is_valid() {
    let src = "enabled = true\ndestinations = [\"local\"]\n";
    let c = parse(src).expect("parse ok");
    c.validate().expect("default schedule must validate");
}

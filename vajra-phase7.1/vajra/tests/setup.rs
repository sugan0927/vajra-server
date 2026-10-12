//! Integration tests for `vajra setup`.

use std::fs;
use std::path::PathBuf;

use vajra::setup::{run, Answers, ScriptedPrompter};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vajra-setup-{tag}-{}-{}",
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

/// Order: domain, email, php, redis, backups, rate_limit, rps.
fn defaults(domain: &str) -> ScriptedPrompter {
    ScriptedPrompter::new(vec![domain, "", "", "", "", "", "50"])
}

#[test]
fn writes_valid_config_with_defaults() {
    let tmp = tmpdir("defaults");
    let cfg = tmp.join("app.toml");
    let a: Answers = run(defaults("example.com"), &cfg).expect("wizard ok");

    assert_eq!(a.domain, "example.com");
    assert!(a.enable_backups);
    assert!(a.enable_rate_limit);
    assert!(!a.use_redis);

    let raw = fs::read_to_string(&cfg).unwrap();
    assert!(raw.contains("[server]"));
    assert!(raw.contains("[[proxy]]"));
    assert!(raw.contains("[backup]"));
    assert!(raw.contains("[rate_limit]"));

    // Round-trip through the config parser. Two adjustments:
    //   * `Settings::from_toml_str` probes `static.root` for existence,
    //     so swap the hardcoded `/var/www/html` for a real temp dir.
    //     This keeps the round-trip meaningful without requiring a
    //     global, root-level path to exist on every test host.
    //   * The second argument is the base dir for relative paths; "/"
    //     is fine for a config that only uses absolute paths.
    let html_dir = tmp.join("html");
    fs::create_dir_all(&html_dir).unwrap();
    let raw_for_parse = raw.replace("/var/www/html", &html_dir.to_string_lossy());

    let _: vajra::config::Settings =
        vajra::config::Settings::from_toml_str(&raw_for_parse, std::path::Path::new("/"))
            .expect("generated TOML must parse as Settings");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn redis_yes_adds_comment_block() {
    let tmp = tmpdir("redis");
    let cfg = tmp.join("app.toml");
    let p = ScriptedPrompter::new(vec!["example.com", "", "", "y", "", "", "50"]);
    let _ = run(p, &cfg).unwrap();
    let raw = fs::read_to_string(&cfg).unwrap();
    assert!(
        raw.contains("Redis detected"),
        "redis comment missing: {raw}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn no_backup_omits_section() {
    let tmp = tmpdir("nobackup");
    let cfg = tmp.join("app.toml");
    let p = ScriptedPrompter::new(vec!["example.com", "", "", "", "n", "", "50"]);
    let _ = run(p, &cfg).unwrap();
    let raw = fs::read_to_string(&cfg).unwrap();
    assert!(!raw.contains("[backup]"), "unexpected [backup]: {raw}");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn existing_config_requires_confirmation() {
    let tmp = tmpdir("overwrite");
    let cfg = tmp.join("app.toml");
    fs::write(&cfg, "# pre-existing\n").unwrap();

    let p = ScriptedPrompter::new(vec!["example.com", "", "", "", "", "", "50", "n"]);
    let err = run(p, &cfg).expect_err("must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), "# pre-existing\n");

    let p = ScriptedPrompter::new(vec!["example.com", "", "", "", "", "", "50", "y"]);
    let _ = run(p, &cfg).expect("overwrite allowed");
    assert!(fs::read_to_string(&cfg).unwrap().contains("[server]"));
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn empty_domain_is_rejected() {
    let tmp = tmpdir("nodomain");
    let cfg = tmp.join("app.toml");
    let p = ScriptedPrompter::new(vec!["", ""]);
    let err = run(p, &cfg).expect_err("empty domain must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let _ = fs::remove_dir_all(&tmp);
}

//! Integration tests for `vajra wp`.
//!
//! These tests touch `PATH` / `VAJRA_WP_PATH`, so a single global
//! mutex serialises them. Run with `cargo test --test wp` normally.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Create a fake `wp` shell script that appends its argv (one arg per
/// line) to `log`. Returns the directory containing it.
fn fake_wp(dir: &std::path::Path, log: &std::path::Path) -> PathBuf {
    let bin = dir.join("wp");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {log}\n",
        log = log.display()
    );
    std::fs::write(&bin, script).unwrap();
    let mut p = std::fs::metadata(&bin).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&bin, p).unwrap();
    bin
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vajra-wp-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Run `f` with `PATH` prefixed by `prefix`. Serialised by ENV_LOCK.
fn with_path_prefix<R>(prefix: &std::path::Path, f: impl FnOnce() -> R) -> R {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let old_nosudo = std::env::var_os("VAJRA_WP_NO_SUDO");
    let mut parts: Vec<PathBuf> = vec![prefix.to_path_buf()];
    parts.extend(std::env::split_paths(&old_path));
    let new = std::env::join_paths(parts).unwrap();
    std::env::set_var("PATH", &new);
    // Tests exercise the direct-exec path, not `sudo -u www-data`. The
    // sudo path requires sudo + the www-data user to exist, which the
    // CI image cannot guarantee.
    std::env::set_var("VAJRA_WP_NO_SUDO", "1");
    let r = f();
    std::env::set_var("PATH", &old_path);
    match old_nosudo {
        Some(v) => std::env::set_var("VAJRA_WP_NO_SUDO", v),
        None => std::env::remove_var("VAJRA_WP_NO_SUDO"),
    }
    r
}

#[test]
fn passes_args_through_verbatim() {
    let tmp = tmpdir("passthrough");
    let log = tmp.join("argv.log");
    fake_wp(&tmp, &log);

    let args = ["plugin", "list"].map(String::from);
    let code = with_path_prefix(&tmp, || vajra::wp::run(&args));
    assert_eq!(code, 0);

    let raw = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines, vec!["plugin", "list"], "log was: {raw:?}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn injects_path_from_env_when_missing() {
    let tmp = tmpdir("inject");
    let log = tmp.join("argv.log");
    fake_wp(&tmp, &log);

    let site = "/var/www/site";
    let args = ["plugin", "install", "woocommerce"].map(String::from);

    let code = with_path_prefix(&tmp, || {
        std::env::set_var(vajra::wp::PATH_ENV, site);
        let c = vajra::wp::run(&args);
        std::env::remove_var(vajra::wp::PATH_ENV);
        c
    });
    assert_eq!(code, 0);

    let raw = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        lines,
        vec![
            format!("--path={site}").as_str(),
            "plugin",
            "install",
            "woocommerce",
        ],
        "log was: {raw:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn explicit_path_wins_over_env() {
    let tmp = tmpdir("explicit");
    let log = tmp.join("argv.log");
    fake_wp(&tmp, &log);

    let args = ["--path=/srv/explicit", "core", "update"].map(String::from);

    let code = with_path_prefix(&tmp, || {
        std::env::set_var(vajra::wp::PATH_ENV, "/var/www/ignored");
        let c = vajra::wp::run(&args);
        std::env::remove_var(vajra::wp::PATH_ENV);
        c
    });
    assert_eq!(code, 0);

    let raw = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        lines,
        vec!["--path=/srv/explicit", "core", "update"],
        "log was: {raw:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn missing_wp_returns_127() {
    let tmp = tmpdir("missing"); // empty dir, no `wp` inside
    let args = ["core".into(), "version".into()];
    let code = with_path_prefix(&tmp, || vajra::wp::run(&args));
    assert_eq!(code, 127);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn db_export_subcommand_works() {
    let tmp = tmpdir("db-export");
    let log = tmp.join("argv.log");
    fake_wp(&tmp, &log);

    let args = ["db", "export", "/var/backups/site.sql"].map(String::from);
    let code = with_path_prefix(&tmp, || vajra::wp::run(&args));
    assert_eq!(code, 0);

    let raw = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(raw.contains("db\n"), "log was: {raw:?}");
    assert!(raw.contains("export\n"), "log was: {raw:?}");
    assert!(raw.contains("/var/backups/site.sql\n"), "log was: {raw:?}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn core_update_subcommand_works() {
    let tmp = tmpdir("core-update");
    let log = tmp.join("argv.log");
    fake_wp(&tmp, &log);

    let args = ["core", "update"].map(String::from);
    let code = with_path_prefix(&tmp, || vajra::wp::run(&args));
    assert_eq!(code, 0);

    let raw = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(raw, "core\nupdate\n");

    let _ = std::fs::remove_dir_all(&tmp);
}

// Suppress an unused-import warning when no test uses Write.
#[allow(dead_code)]
fn _unused_write_marker<W: Write>(_: &W) {}

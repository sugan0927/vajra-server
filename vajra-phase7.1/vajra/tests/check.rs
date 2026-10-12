//! Integration tests for `vajra --check`.

use vajra::check::{self, Severity};
use vajra::config::Settings;

fn scopes(r: &check::Report, sev: Severity) -> Vec<&'static str> {
    r.findings
        .iter()
        .filter(|f| f.severity == sev)
        .map(|f| f.scope)
        .collect()
}

#[test]
fn clean_default_has_no_errors() {
    let r = check::run(&Settings::default());
    assert_eq!(
        r.errors(),
        0,
        "defaults must not produce errors: {:#?}",
        r.findings
    );
    assert!(r.checks_run >= 5, "checks_run too low: {}", r.checks_run);
}

#[test]
fn loopback_admin_is_info_not_warning() {
    let s = Settings {
        admin: Some("127.0.0.1:9100".parse().unwrap()),
        ..Settings::default()
    };
    let r = check::run(&s);
    assert!(!scopes(&r, Severity::Warning).contains(&"[admin]"));
    assert!(scopes(&r, Severity::Info).contains(&"[admin]"));
}

#[test]
fn non_loopback_admin_is_warning() {
    let s = Settings {
        admin: Some("0.0.0.0:9100".parse().unwrap()),
        ..Settings::default()
    };
    let r = check::run(&s);
    assert!(scopes(&r, Severity::Warning).contains(&"[admin]"));
}

#[test]
fn tiny_grace_is_warning() {
    let s = Settings {
        grace_secs: 1,
        ..Settings::default()
    };
    let r = check::run(&s);
    assert!(scopes(&r, Severity::Warning).contains(&"[server]"));
}

#[test]
fn backup_disabled_is_info() {
    let r = check::run(&Settings::default());
    assert!(scopes(&r, Severity::Info).contains(&"[backup]"));
}

#[test]
fn render_ends_with_summary() {
    let r = check::run(&Settings::default());
    let mut buf = Vec::new();
    r.render(&mut buf).unwrap();
    let text = String::from_utf8(buf).unwrap();
    assert!(text.contains("OK:"), "missing summary: {text}");
}

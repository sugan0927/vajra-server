//! Render-level tests for the Prometheus exposition output.
//! `init()` uses a global registry (OnceLock), so all tests share it.
//! `render()` takes no args and reads from the global registry.

use std::collections::BTreeSet;

fn ensure_init() {
    // Idempotent — first call sets up the global registry.
    let _ = vajra::metrics::init();
}

/// Prometheus histograms emit `# HELP name` + `# TYPE name histogram`, but
/// the samples are `name_bucket`, `name_sum`, `name_count`. Similarly for
/// summaries. Normalize a sample name back to its family.
fn family_of(sample: &str) -> &str {
    for suffix in ["_bucket", "_sum", "_count"] {
        if let Some(base) = sample.strip_suffix(suffix) {
            return base;
        }
    }
    sample
}

#[test]
fn every_series_has_help_and_type() {
    ensure_init();
    let text = vajra::metrics::render();
    assert!(!text.is_empty(), "render() returned empty — init() failed?");

    let mut help: BTreeSet<String> = BTreeSet::new();
    let mut typ: BTreeSet<String> = BTreeSet::new();
    let mut samples: BTreeSet<String> = BTreeSet::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            help.insert(rest.split_whitespace().next().unwrap().to_string());
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            typ.insert(rest.split_whitespace().next().unwrap().to_string());
        } else if !line.starts_with('#') && !line.is_empty() {
            let name = line.split(['{', ' ']).next().unwrap().to_string();
            samples.insert(name);
        }
    }

    for s in &samples {
        let fam = family_of(s);
        assert!(
            help.contains(fam),
            "series `{s}` (family `{fam}`) missing # HELP"
        );
        assert!(
            typ.contains(fam),
            "series `{s}` (family `{fam}`) missing # TYPE"
        );
    }
}

#[test]
fn documented_families_are_present() {
    ensure_init();
    let text = vajra::metrics::render();

    let expected = [
        "vajra_build_info",
        "vajra_uptime_seconds",
        "vajra_requests_total",
        "vajra_request_duration_seconds",
        "vajra_active_connections",
        "vajra_requests_in_flight",
        "vajra_php_fpm_requests_total",
        "vajra_php_fpm_duration_seconds",
        "vajra_bytes_sent_total",
        "vajra_bytes_received_total",
        "vajra_config_reloads_total",
        "vajra_config_reload_errors_total",
    ];

    for name in expected {
        assert!(
            text.contains(&format!("# HELP {name} ")),
            "missing family `{name}`"
        );
    }
}

#[test]
fn fpm_status_label_covers_all_outcomes() {
    ensure_init();
    let text = vajra::metrics::render();

    for st in ["ok", "error", "timeout", "parse"] {
        let needle = format!(r#"status="{st}""#);
        assert!(text.contains(&needle), "missing FPM series for {st}");
    }
}

#[test]
fn render_is_newline_terminated() {
    ensure_init();
    let text = vajra::metrics::render();
    assert!(text.ends_with('\n'), "output must end with \\n");
    assert!(!text.contains('\0'), "output must not contain NUL");
}

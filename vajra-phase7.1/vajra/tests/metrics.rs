use std::time::Duration;
use vajra::metrics;

#[test]
fn metrics_end_to_end() {
    metrics::init().expect("metrics init");
    let fpm = metrics::fpm().expect("fpm metrics registered");
    assert_eq!(fpm.requests.len(), 4);
    assert_eq!(fpm.durations.len(), 4);

    for (idx, name) in metrics::FPM_STATUSES.iter().enumerate() {
        let before = fpm.requests[idx].get();
        fpm.observe(idx, Duration::from_millis(10));
        assert_eq!(fpm.requests[idx].get(), before + 1, "status={name}");
    }

    let text = metrics::render();
    for needle in [
        "vajra_php_fpm_requests_total",
        "vajra_php_fpm_duration_seconds",
        "vajra_bytes_sent_total",
        "vajra_bytes_received_total",
        r#"status="ok""#,
        r#"status="error""#,
        r#"status="timeout""#,
        r#"status="parse""#,
    ] {
        assert!(text.contains(needle), "missing {needle:?}");
    }

    metrics::record_bytes_sent(1_000_000);
    metrics::record_bytes_received(500_000);
    metrics::flush_bytes();

    fpm.observe(999, Duration::from_secs(1));
}

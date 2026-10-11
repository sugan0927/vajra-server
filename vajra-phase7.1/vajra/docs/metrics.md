# Vajra Metrics Reference

सभी metrics `/metrics` endpoint पर Prometheus format में expose होते हैं
(`[admin].listen`, default `127.0.0.1:9200`)।

## PHP-FPM / FastCGI

### vajra_php_fpm_requests_total{status="..."}

PHP-FPM (FastCGI) upstream को भेजी गई requests का counter, outcome के हिसाब से।

| status | कब बढ़ता है | Code path |
|---|---|---|
| ok | FastCGI stream पूरा decode हुआ और HTTP response बना | worker.rs::proxy_done() |
| error | Connection refused, DNS fail, या 5xx-level upstream failure | worker.rs::proxy_fail(idx, 502) |
| timeout | [php].timeout_secs expire हुआ (504) | worker.rs::proxy_fail(idx, 504) |
| parse | FastCGI protocol decode fail (feed() ने error दिया) | worker.rs decode-error hook (line ~1930) |

### vajra_php_fpm_duration_seconds{status="..."}

Histogram — proxy job create होने से response पूरा होने तक का wall time.
Buckets: 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10.

p95 के लिए PromQL:

    histogram_quantile(0.95,
      sum by (le) (rate(vajra_php_fpm_duration_seconds_bucket{status="ok"}[5m])))

## Bytes

### vajra_bytes_sent_total

Clients को लिखे गए total bytes (response head + body)।

### vajra_bytes_received_total

Clients से पढ़े गए total bytes (request head + body)।

दोनों counters atomic fast-path use करते हैं — hot path में AtomicU64::fetch_add,
और हर ~1 MiB पर (या render के वक़्त) prometheus IntCounter में flush होते हैं।
इसलिए /metrics scrape पर latest values दिखती हैं।

## Implementation notes

- FpmMetrics एक OnceLock में रहता है, metrics::init() पर register होता है।
- ProxyJob में started: Instant और fpm_observed: bool fields हैं — fpm_observed
  double-count रोकता है जब proxy_fail और proxy_done दोनों call हो सकते हैं।
- Bytes counters http::process() में wrapped हैं (process_inner pattern)।
- FastCGI decode error पर status="parse" observe होता है, और fpm_observed = true
  set किया जाता है ताकि बाद का proxy_fail भी double-count न करे।

## Alerts

/etc/vajra/alerts/vajra.yaml — Prometheus rule file।
Production में prometheus.yml के rule_files: में यह path include करें:

    rule_files:
      - /etc/vajra/alerts/vajra.yaml

Rules:

| Alert | Severity | Threshold |
|---|---|---|
| VajraFpmErrorRateHigh | warning | rate(error[5m]) > 0.1 |
| VajraFpmTimeoutRateHigh | critical | rate(timeout[5m]) > 0.05 |
| VajraFpmParseErrors | warning | rate(parse[5m]) > 0 |
| VajraFpmP95LatencyHigh | warning | p95 > 1s |
| VajraFpmAllFailing | critical | ok=0 और error>0 |
| VajraNoTraffic | info | 10m कोई traffic नहीं |
| VajraBandwidthSpike | warning | egress > 50 MiB/s |

## Verification

Integration test: tests/metrics.rs — cargo test --test metrics

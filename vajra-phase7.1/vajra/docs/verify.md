# Verify — Metrics Endpoint

Admin listener default: `127.0.0.1:9202`.

Set this first in every shell session:

    ADMIN=127.0.0.1:9202

## 0. Reachable + content-type

    curl -sS -D - -o /dev/null "http://$ADMIN/metrics" \
      | grep -iE '^HTTP/|^content-type:'

Expected:

    HTTP/1.1 200 OK
    Content-Type: text/plain; version=0.0.4; charset=utf-8

## 1. Build + uptime

    curl -s "http://$ADMIN/metrics" | grep '^vajra_build_info'
    curl -s "http://$ADMIN/metrics" | grep '^vajra_uptime_seconds'

## 2. HTTP metrics

    # 10 methods × 5 classes = 50 series
    curl -s "http://$ADMIN/metrics" | grep -c '^vajra_requests_total'

    # Move a counter
    curl -s -o /dev/null "http://127.0.0.1:8080/health"
    curl -s "http://$ADMIN/metrics" \
      | grep 'vajra_requests_total{method="GET",status_class="2xx"}'

    # Histogram buckets
    curl -s "http://$ADMIN/metrics" \
      | grep '^vajra_request_duration_seconds_bucket' | head -3

    # Gauges
    curl -s "http://$ADMIN/metrics" | grep '^vajra_active_connections'
    curl -s "http://$ADMIN/metrics" | grep '^vajra_requests_in_flight'

## 3. PHP-FPM — all 4 statuses

    for st in ok error timeout parse; do
      printf '%-8s ' "$st"
      curl -s "http://$ADMIN/metrics" \
        | grep -c "vajra_php_fpm_requests_total{status=\"$st\"}"
    done

Expected: each line ends with `1`.

    curl -s "http://$ADMIN/metrics" \
      | grep '^vajra_php_fpm_duration_seconds_count' | sort

Expected: 4 lines (one per status).

## 4. Bytes counters

    curl -s "http://$ADMIN/metrics" | grep '^vajra_bytes_sent_total'
    curl -s "http://$ADMIN/metrics" | grep '^vajra_bytes_received_total'

## 5. Config reload counters

    curl -s "http://$ADMIN/metrics" | grep '^vajra_config_reload'

    # Trigger a reload, counter should increment
    curl -sS -X POST "http://$ADMIN/reload"
    curl -s "http://$ADMIN/metrics" | grep '^vajra_config_reloads_total'

## 6. Process metrics (prometheus `process` feature)

    curl -s "http://$ADMIN/metrics" \
      | grep -E '^process_(cpu|resident|virtual|open|max|start)'

## 7. All families present — one-liner

    curl -s "http://$ADMIN/metrics" \
      | awk '/^# HELP/ {print $3}' | sort -u > /tmp/vajra-fam.txt

    for want in \
      vajra_build_info vajra_uptime_seconds \
      vajra_requests_total vajra_request_duration_seconds \
      vajra_active_connections vajra_requests_in_flight \
      vajra_php_fpm_requests_total vajra_php_fpm_duration_seconds \
      vajra_bytes_sent_total vajra_bytes_received_total \
      vajra_config_reloads_total vajra_config_reload_errors_total
    do
      grep -qx "$want" /tmp/vajra-fam.txt \
        && echo "ok   $want" \
        || echo "MISS $want"
    done

## 8. Prometheus format sanity (optional)

    curl -s "http://$ADMIN/metrics" | promtool check metrics

Expected: no output.

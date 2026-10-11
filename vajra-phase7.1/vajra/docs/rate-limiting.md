# Rate limiting — design

**Status:** design only. src/rate_limit.rs has types + signatures;
RateLimiter::check is not yet called from the request path.

## Goals

- Bound per-client rate with smooth rate + hard burst cap.
- Zero-lock hot path, no background threads.
- Bounded memory under attack.
- Reuse effective-client-IP plumbing from [access].

## Non-goals

- Global cross-worker exact enforcement; use nginx/Envoy for that.
- Distributed limiting across Vajra instances.
- Fairness, weighted quotas, per-route budgets.
- Adaptive limiting from traffic signals.

## Algorithm - token bucket

Per tracked client IP: capacity = burst, refill = rps / worker,
initial tokens = burst.

Refill lazily: tokens = min(burst, tokens + elapsed * rps).
If tokens >= 1: consume, allow. Else deny with
Retry-After = ceil((1 - tokens) / rps) seconds.

Why not sliding window: O(requests/window) memory, needs sorted
structure, expensive at scale.
Why not fixed window: burst doubling at window boundary.

## Data structures

RateLimiter { settings, buckets: HashMap<IpAddr, Bucket>, last_cleanup }
Bucket      { tokens: f64, last_refill: Instant, last_seen: Instant }

~100 B/IP, ~10 MB per worker at max_tracked_ips = 100_000.

## Concurrency model

Per-worker sharded (option B): lock-free, each worker gets rps / N
and full burst. Trade-off: a client spraying across workers can
momentarily get N x burst tokens; sustained converges to rps.
Rejected global mutex: serializes all requests, kills io_uring gains.

Two consequences:
- rps in config is global; each worker gets rps / N.
- A single client pinned to one worker sees rps / N.

## Interaction with [access]

Order: parse headers -> [access] (403 on deny) -> [rate_limit]
(429 + Retry-After on deny) -> route.

Rationale:
- [access] is a hard gate; denied IPs should not consume budget.
- [rate_limit] then only sees traffic you would serve; 429 is
  unambiguous.
- Both use same effective client IP.

## Config reference

[rate_limit]
enabled                = true
requests_per_second    = 50
burst                  = 100
whitelist_ips          = ["127.0.0.1", "10.0.0.0/8"]
max_tracked_ips        = 100_000
cleanup_interval_secs  = 60

Fields:
- enabled                bool   default false
- requests_per_second    u32    default 0
- burst                  u32    default 0
- whitelist_ips          CIDR[] default []
- max_tracked_ips        usize  default 0
- cleanup_interval_secs  u64    default 0

Validation (fatal at startup):
- enabled -> requests_per_second >= 1
- enabled -> burst >= 1
- enabled -> max_tracked_ips >= 1
- whitelist entries must parse as CIDR

Soft warnings (logged only):
- cleanup_interval_secs == 0 with enabled = true
- burst < requests_per_second

## Memory bounds

1. Cap: buckets.len() >= max_tracked_ips -> new IPs denied
   (fail-closed). Sustained attacks tighten rather than degrade.
2. Cleanup: evict last_seen older than 2 x (burst / rps) seconds,
   triggered on the cleanup_interval_secs boundary.
3. No insertion on whitelisted IPs -> never inflates buckets.

Operator signal: rate_limit_cap_reached_total counter (Day 12).

## Response semantics

Deny:
- Status 429.
- Header Retry-After: <n> (whole seconds, >= 1).
- Empty body.

Allow:
- No headers added. Rate limiting is invisible to successful
  requests; no X-RateLimit-* leakage.

## Observability

- rate_limit_denied_total         counter, per denied request.
- rate_limit_cap_reached_total    counter, when cap is hit.
- rate_limit_buckets              gauge, sampled at cleanup boundary.
- First denial per IP logged at info; later denials suppressed until
  the next cleanup window.

## Testing plan (Day 12)

Unit tests (tests/rate_limit.rs):
1. disabled_limiter_always_allows
2. burst_then_throttle
3. refill_over_time
4. burst_cap_enforced
5. whitelisted_ip_not_tracked
6. retry_after_is_ceil_and_at_least_1
7. cap_reached_denies_new_ip
8. cleanup_evicts_idle_buckets
9. config_validation (enabled with rps = 0 fails; bad CIDR fails)

Integration tests (tests/rate_limit_e2e.rs, Day 12):
- 5 requests at rps=1 burst=3 -> first 3 pass, 4th and 5th get 429
  with Retry-After.
- Same with [access] deny-list also present -> access wins, 403 not
  429.

## Open questions (Day 12)

1. Global mode: add shared = true with Arc<Mutex<RateLimiter>>?
   Deferred; start with per-worker.
2. Per-route overrides [[rate_limit.overrides]] with path, rps,
   burst. Likely Day 13.
3. Connection affinity: does a Vajra connection stick to one worker?
   If yes, N x burst bound is real; else 1 x burst in practice.
4. IPv6 /64 aggregation. Deferred; document current per-address
   behavior explicitly.
5. Metrics plumbing: confirm whether the metrics module exposes
   atomic-global counters or per-worker counters.

## Implementation note (Day 12)

Each worker holds its own `RateLimiter` and uses the full configured
`requests_per_second` — the `rps / N` division described above is
aspirational and not yet implemented. The effective global rate is
therefore `N x requests_per_second` in the worst case for a client
spraying across workers; the common single-worker case is exact. To
target a specific global rate X, set `requests_per_second = X / N`.

Reload preserves existing buckets and only refreshes
`RateLimiter::settings`.

## Update — IPv6 /64 aggregation + soft warnings (Day 12.1)

### `aggregate_ipv6_64` (default `false`)

When enabled, IPv6 client addresses are masked to their /64 prefix
before bucket lookup, so all addresses within one /64 share a single
token bucket. This bounds per-client memory when a single attacker
rotates source addresses inside their delegated /64.

- Whitelist matching is unaffected: it always runs against the
  *original* address, before aggregation. Whitelisting
  `2001:db8::1` whitelists exactly that address, not its /64.
- IPv4 keys are unchanged.
- Default `false` for backward compatibility; flip it in
  `[rate_limit]` to opt in.

    [rate_limit]
    aggregate_ipv6_64 = true

### Soft warnings at startup

`build_rate_limit` now emits `eprintln!` warnings (fatal validation is
unchanged) for:

- `cleanup_interval_secs = 0` while `enabled = true` — buckets grow
  until `max_tracked_ips` is hit.
- `burst < requests_per_second` — sustained throughput is burst-limited.

Logs only; they do not fail startup.

## Update — Day 13: metric names aligned to spec

Renamed and extended the rate-limit metric set. Dashboards written
against the day-12 names must be updated; the semantic behaviour is
unchanged.

| Old (day 12)                          | New (day 13)                          | Meaning |
|---------------------------------------|---------------------------------------|---------|
| —                                     | `vajra_rate_limit_allowed_total`      | requests permitted (successful token consume *or* whitelist bypass) |
| `vajra_rate_limit_denied_total`       | `vajra_rate_limit_rejected_total`     | requests refused with 429, including cap-reached |
| `vajra_rate_limit_buckets`            | `vajra_rate_limit_tracked_ips`        | gauge, current bucket count |
| `vajra_rate_limit_cap_reached_total`  | *(unchanged)*                         | rejections due to `max_tracked_ips` |

`allowed` is incremented only inside `check()` — the disabled fast path
and `ctx.rate_limit == None` both skip the counters entirely.

## Update — Day 14: effective client IP for both gates

The access check and the rate limiter now share one effective-client
computation per request:

    peer   = ctx.client_ip        (socket peer, always set in prod)
    client = access.effective_client_ip(peer, XFF, X-Real-IP)
             or peer, if [access] is absent

Consequences:
- Buckets, deny-logs, and `[access]` all key on the same identity.
- When `ctx.client_ip` is `None` (only tests do this), *both* checks
  are skipped entirely. Production never sees a `None` here.

This restores the pre-Day-14 invariant that access was evaluated
independently of the rate limiter; if `ctx.client_ip` is set, both run.

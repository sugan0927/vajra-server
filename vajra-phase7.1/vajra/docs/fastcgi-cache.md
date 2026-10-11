# FastCGI cache - design

**Status:** design only. No code yet. Day 18 lands the storage layer;
Days 19+ wire it into the request path.

## Goals

- Cache PHP-FPM responses (WordPress, generic FastCGI) on disk.
- LRU + size-bounded so the VPS never runs out of disk.
- Bypass rules keyed on cookie and path, matching nginx
  fastcgi_cache_bypass conventions.
- Zero allocation on miss beyond what the request already does.

## Non-goals

- Distributed cache across machines.
- Cache stampede protection (single-flight). Deferred; noted below.
- Byte-range requests.
- HTTP/2 push.

## Config schema

    [fastcgi_cache]
    enabled          = true
    path             = "/var/cache/vajra/fcgi"
    size_mb          = 1024
    ttl_secs         = 600
    max_entry_size   = 1048576
    bypass_cookie    = ["wordpress_logged_in_*", "wp-postpass_*"]
    exclude_paths    = ["/wp-admin/*", "/wp-login.php", "/wp-json/*"]

Fields:

| name             | type     | default                    | meaning |
|------------------|----------|----------------------------|---------|
| enabled          | bool     | false                      | master switch |
| path             | string   | /var/cache/vajra/fcgi      | on-disk root |
| size_mb          | u64      | 1024                       | total bytes budget |
| ttl_secs         | u64      | 600                        | entry lifetime |
| max_entry_size   | u64      | 1048576                    | per-entry size cap |
| bypass_cookie    | string[] | []                         | glob on cookie name |
| exclude_paths    | string[] | []                         | glob on request path |

Fatal validation (when enabled = true):
- path must be non-empty, absolute, shorter than 200 bytes.
- size_mb >= 1, ttl_secs >= 1, max_entry_size >= 1.

Soft warning:
- path missing at startup - created with mkdir -p.

## Cache key

FastCGI cache key = concatenation of:

    "v1\0" + method + "\0" + host + "\0" + path [+ "\0" + query]

- method - only GET and HEAD are eligible; other methods always bypass.
- host - from the Host header, lowercased.
- path - URL path without query string.
- query - included when present, except for a configured ignore list.
  Day 19 will add [fastcgi_cache] ignore_query = ["fbclid", "utm_*"]
  to drop known tracking params.
- The "v1\0" prefix is a cache-format version; bumping invalidates
  everything without a data migration.

The key is stored alongside the body and verified on read, so a
collision in the 128-bit hash is a cache miss, never wrong content.

## Storage layout

    <path>/
      index.bin
      data/
        aa/
          <32-hex-fnv128>
        ab/
          ...

- Two-level shard (aa/) keeps any single directory small even with
  millions of entries; the shard is the first two hex characters.
- File contents = the response body verbatim. All metadata lives in
  the index; the hot read path is open + read.

### Index

index.bin is a single flat file, rewritten atomically at each
mutation batch:

    magic  : "VJFC1\0\0\0"      (8 bytes)
    count  : u64 LE
    repeated count times:
      key_len     : u32 LE
      key         : bytes
      size        : u64 LE       # body length
      expires_at  : u64 LE       # unix secs
      last_access : u64 LE       # unix secs (LRU)

Written to index.bin.tmp, fsync, then rename(2). A crash never leaves
a partial index visible.

In memory, the index is a Vec<IndexEntry> plus a HashMap<key, idx> for
O(1) key lookup. On get/put the LRU last_access field is updated; the
map is not re-sorted.

## Eviction

Three triggers:

1. put over size: after writing the new entry, while
   total_bytes > size_mb * 1_048_576, evict the entry with the smallest
   last_access (linear scan; a min-heap rebuild every N evictions once
   index.len() > 4096).
2. Expiry on read: a lookup whose expires_at <= now returns a miss
   and removes the entry.
3. Periodic sweep: cleanup_expired() called manually in v1 (no
   background thread).

LRU is approximate - last_access is updated on get without a global
ordering structure, so heavy concurrent access produces a slightly
stale order. Same trade-off nginx makes.

## Request flow (Day 19+)

    on FastCGI request:
      1. if !settings.enabled                     -> miss
      2. if method not in {GET, HEAD}             -> miss
      3. if path matches exclude_paths            -> miss
      4. if any request Cookie name matches a
           bypass_cookie glob                    -> miss
      5. key = build_key(...)
      6. if cache.get(key) -> HIT: write cached status + headers + body
      7. else -> MISS:
           a. build upstream request
           b. on success, if response is cacheable
              (status in {200, 301, 302, 404}, no Set-Cookie, no
               Cache-Control: no-store|private)
                 cache.put(key, body)
           c. write response to client

Cache hits get X-FastCGI-Cache: HIT; miss and store gets MISS; a
stored-but-bypassed response gets BYPASS.

## Observability

Counters (Prometheus, added Day 19):

- vajra_fastcgi_cache_requests_total{result="hit|miss|bypass"}
- vajra_fastcgi_cache_entries        (gauge, sampled at cleanup)
- vajra_fastcgi_cache_bytes          (gauge, sampled at cleanup)
- vajra_fastcgi_cache_evictions_total

## Open questions (Day 18/19)

1. Stampede protection. Two clients hit a cold key at the same time;
   both go upstream. Nginx solves this with a per-key lock. Deferred:
   v1 accepts a duplicate upstream request.
2. Index write frequency. Rewrite on every put is simple and correct,
   but a busy cache will rewrite the index thousands of times per
   second. Two mitigations, both deferred:
   - batch index writes at most once per second
   - append-only journal + periodic compaction
3. Set-Cookie responses. v1 will not store any response that carries
   Set-Cookie. Some sites set a session cookie on every page (bad
   practice); those pages simply will not cache.
4. purge endpoint. A PURGE /path HTTP method handled by the worker to
   invalidate a single key or prefix. Deferred to Day 20.
5. Persistence across restart. The index is reloaded from disk on
   worker start, so entries survive restart. Data files whose index
   entries are missing are considered orphaned and swept at first
   cleanup.

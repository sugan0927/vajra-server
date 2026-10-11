# Compression - design

Brotli on-the-fly compression. Default off; enabled from [compression].

## Config

    [compression]
    brotli   = true
    level    = 6
    min_size = 1024

level > 11 is fatal; min_size = 0 warns.

## Gates

Compress iff all: enabled; not HEAD; len >= min_size; CT not pre-compressed;
Accept-Encoding negotiates br.

## Skip list

image/* except svg+xml; video/*; audio/*; zip gzip zstd rar 7z bzip2 pdf;
font woff woff2 ttf otf; ms-fontobject; x-font-ttf; x-font-otf.

## Response shape

Content-Encoding: br; Vary: Accept-Encoding; Content-Length = compressed size.

## HEAD

Skip; Content-Length stays uncompressed.

## Metrics

Not in Day 15.

## Hot path (Day 15)

Compression is wired into `write_reply()` for `Reply::Mem` only.
`Reply::File` (streamed static files) and proxy responses pass through
unchanged.

- `Ctx.compression: Option<&CompressionSettings>` is set by the worker
  from its live settings; `None` disables compression for that request
  (used by unit tests).
- `Accept-Encoding` is parsed once per request in the header loop.
- `maybe_compress()` returns `(bytes, extra_headers)`:
  * `enabled == false` or `None` → identity, no headers.
  * `is_pre_compressed(ctype)` → identity, no headers (the CT is a
    signal to *all* intermediaries to skip, not just us).
  * Otherwise, `Vary: Accept-Encoding` is added unconditionally, and
    `Content-Encoding: br` is added only when the body actually got
    smaller. `Content-Length` always matches the bytes on the wire.

## Scope and follow-ups

Not in Day 15:

- **File / proxy responses.** A future pass could compress at the
  source (a brotli writer around `OpenFile`) or in `write_proxy_response`.
- **Streaming.** Vajra buffers Mem replies fully; the compressed length
  is known before the head is written.
- **Chunked transfer.** Not used for Mem replies today.
- **Zstd.** Title says Zstd, spec says Brotli — we shipped Brotli. Zstd
  would be a parallel `negotiate()` arm and a second encoder.
- **Metrics.** See the "Metrics" section above.

## Testing plan (Day 15)

Unit tests (`src/compress.rs`): negotiation, skip-list, gate matrix,
round-trip, config validation.

Hot-path tests (`src/http.rs::compress_hot_path_tests`, 8 tests):

1. `br_negotiated_compresses_mem_body` — br → Content-Encoding: br,
   Vary, smaller Content-Length.
2. `no_accept_encoding_serves_identity` — no header → identity + Vary.
3. `gzip_only_serves_identity` — gzip only → identity + Vary.
4. `br_with_q0_serves_identity` — explicit q=0 → identity.
5. `body_below_min_size_serves_identity` — small body → identity + Vary.
6. `pre_compressed_content_type_skipped` — image/png → identity, no Vary.
7. `disabled_settings_serve_identity` — disabled / None → identity, no
   Vary.
8. `head_request_not_compressed` — HEAD → identity + Vary, uncompressed
   Content-Length.

## Update — Day 16: gzip fallback

Two encoders are now supported. Config gains `gzip` and `gzip_level`:

    [compression]
    brotli      = true
    gzip        = true
    level       = 6         # brotli quality, 0..=11
    gzip_level  = 6         # gzip level, 1..=9
    min_size    = 1024

### Negotiation

`negotiate(Accept-Encoding, settings) -> Encoding`:

1. If neither `brotli` nor `gzip` enabled → `Identity`.
2. Parse `br`, `gzip` / `x-gzip`, `*` q-values (default 1.0).
3. Effective q for a codec = its own q, else `*` q, else 0. A disabled
   codec has effective q 0 regardless of the header.
4. Pick the highest effective q. **Ties favour brotli.**
5. If the winner has q ≤ 0 → `Identity`.

### Response

- brotli → `Content-Encoding: br`
- gzip   → `Content-Encoding: gzip`
- identity → no `Content-Encoding` header.

`Vary: Accept-Encoding` is added on every compressible type regardless
of the outcome, so shared caches key correctly.

### Validation

- `brotli = true` and `level > 11` → fatal.
- `gzip = true` and `gzip_level < 1 | > 9` → fatal.
- `min_size = 0` while either is on → soft warning.

### Tests added (Day 16)

Unit (`src/compress.rs`): negotiate prefers brotli, falls back to gzip,
respects disabled codecs, edge cases (`q=0`, `*`, empty), gzip round-trip,
config validation.

Hot-path (`src/http.rs::compress_hot_path_tests`):
`gzip_served_when_brotli_disabled`, `brotli_preferred_when_both_available`,
`gzip_fallback_when_br_not_offered`, `gzip_only_not_used_when_client_asks_br_only`.

# Security headers

Vajra can inject a fixed set of security-relevant response headers. Configure
them in `[headers]`; they apply to **every** response unless a path override
says otherwise.

## Config

    [headers]
    hsts = "max-age=31536000; includeSubDomains"
    x_frame_options = "DENY"
    x_content_type_options = "nosniff"
    referrer_policy = "strict-origin-when-cross-origin"
    permissions_policy = "geolocation=(), camera=(), microphone=()"

Each key is optional. Omit a key to leave that header **unmanaged** —
Vajra will not strip or replace a header it wasn't asked to manage.

## Per-path overrides

    [[headers.override]]
    path = "/api/"
    hsts = "max-age=60"

    [[headers.override]]
    path = "/embed/"
    x_frame_options = ""            # empty string = REMOVE for this path

- `path` must be absolute (start with `/`).
- Longest-prefix match wins: `/a/b/` beats `/a/` for `/a/b/x`.
- Empty string (`""`) means **remove** this header for this path.
- Omitted keys **inherit** from the top-level `[headers]` block.

## Header name map

| Config key             | Wire header                  |
|------------------------|------------------------------|
| `hsts`                 | `Strict-Transport-Security`  |
| `x_frame_options`      | `X-Frame-Options`            |
| `x_content_type_options` | `X-Content-Type-Options`   |
| `referrer_policy`      | `Referrer-Policy`            |
| `permissions_policy`   | `Permissions-Policy`         |

## Notes

- **HSTS is only meaningful over HTTPS.** If you serve both HTTP and HTTPS
  on the same host, browsers apply the HSTS policy per origin as soon as
  they see it over HTTPS. Set it only when you control the HTTPS side.
- **Managed headers are replaced, not duplicated.** If a response already
  carries `X-Frame-Options` (from an upstream, or from a static handler),
  and `[headers].x_frame_options` is set, the existing value is removed
  and the configured one is written. This is deliberate: no header
  smuggling, no "two X-Frame-Options" confusion.
- **CR/LF are rejected at parse time.** Values must be single-line;
  a stray `\n` is a config error (defense against header injection).
- **No default values.** Vajra ships with no security headers enabled so
  that upgrades don't silently change response behaviour. Add what you
  need.

## Verify

    # All headers on a normal path
    curl -sI http://127.0.0.1:8080/ \
      | grep -iE 'strict-transport|x-frame|x-content|referrer|permissions'

    # Override path
    curl -sI http://127.0.0.1:8080/api/ | grep -i hsts
    # Expect: max-age=60

    # Removal path
    curl -sI http://127.0.0.1:8080/embed/ | grep -i x-frame
    # Expect: no output

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

## Content-Security-Policy (CSP)

CSP is the highest-leverage header in this set — it can neutralise most
reflected-XSS and inline-script injection, *provided the policy matches
your actual site*. Vajra does **not** ship a CSP default; you must opt in.

    [headers]
    content_security_policy = "default-src 'self'"

Per-path override:

    [[headers.overrides]]
    path = "/admin/"
    content_security_policy = "default-src 'self'; frame-ancestors 'none'"

Empty string removes the header for a path:

    [[headers.overrides]]
    path = "/embed/"
    content_security_policy = ""

### WordPress-compatible starting point

The example below works with unmodified WordPress 6.x plus most of the
plugin ecosystem. It is intentionally permissive — `'unsafe-inline'`
and `'unsafe-eval'` are what WordPress needs to function, and `data:` /
`https:` image sources cover avatars and CDN assets. It is **not** a
strict policy; treat it as a floor, not a ceiling.

    [headers]
    content_security_policy = """
    default-src 'self'; \
    script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
    style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: https:; \
    font-src 'self' data:; \
    connect-src 'self'; \
    frame-ancestors 'self'; \
    base-uri 'self'; \
    form-action 'self'
    """

The same string is exposed in code as `vajra::config::WORDPRESS_CSP`,
so if you build a config programmatically you can use it directly:

    use vajra::config::{HeaderSettings, WORDPRESS_CSP};

    let cfg = HeaderSettings {
        content_security_policy: Some(WORDPRESS_CSP.to_string()),
        ..HeaderSettings::default()
    };

### Migrating to a stricter policy

If your site has moved past inline scripts (e.g. via a nonce-emitting
plugin), tighten `script-src` and `style-src` to drop `'unsafe-inline'`
and `'unsafe-eval'`:

    script-src 'self' 'nonce-{NONCE}' 'strict-dynamic';
    style-src  'self' 'nonce-{NONCE}';

Vajra cannot currently inject per-response nonces; that is future work.
Until then, keep the WP-compatible policy or provide your own nonce
plumbing at the application layer.

### Why this is a string, not a "`wordpress`" keyword

An earlier draft made `content_security_policy = "wordpress"` a magic
keyword. We rejected it: a future change to the built-in policy would
then silently change what a running site emits, without a config diff
to review. Users copy the string once and own it.


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

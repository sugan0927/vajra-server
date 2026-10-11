# Access control — IP allow/deny + trusted proxies

Vajra can restrict who reaches a site by source IP. The feature is opt-in:
if there is no `[access]` section, or every list in it is empty, no
filtering occurs and the check is skipped entirely.

## Minimal config

Block a specific network:

    [access]
    deny_ips = ["203.0.113.0/24"]

Allow only two internal ranges and block everything else:

    [access]
    allow_ips = ["10.0.0.0/8", "192.168.0.0/16"]

Block a range **and** allow everyone else (common for stopping a
particular abuse source while keeping the site public):

    [access]
    deny_ips = ["198.51.100.42"]

Deny everything (maintenance lockout — use carefully):

    [access]
    deny_ips = ["0.0.0.0/0", "::/0"]

## Semantics

Evaluation order for the effective client IP:

1. If it matches any `deny_ips` entry → **403 Forbidden**, full stop.
2. Else if `allow_ips` is non-empty and the IP matches none of its
   entries → **403 Forbidden**.
3. Else → **allow**.

Two consequences worth stating explicitly:

* **Deny always beats allow.** If `10.1.2.3` is in both lists, it is
  denied.
* **An empty `allow_ips` does not deny anyone.** It only means "no
  whitelist is in effect". This is the standard firewall reading of an
  empty list, and the opposite of the intuition some people bring from
  ACL systems.

## CIDR syntax

Each list entry is parsed by the [`ipnet`](https://docs.rs/ipnet) crate.

| Form             | Meaning                                  |
|------------------|------------------------------------------|
| `10.0.0.0/8`     | IPv4 network, /8 prefix                  |
| `203.0.113.7`    | Single IPv4 host (implicit `/32`)        |
| `2001:db8::/32`  | IPv6 network                             |
| `::1`            | Single IPv6 host (implicit `/128`)       |

IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) are normalized to plain
IPv4 before matching, so a `10.0.0.0/8` rule matches a client presented
to the server as `::ffff:10.1.2.3`. This matters on Linux hosts with
`net.ipv6.bindv6only=0` where IPv4 clients arrive on an IPv6 socket.

Malformed entries cause a **startup failure** with the offending field
and value in the error message — the server will not boot with a broken
`[access]` section.

## Trusted proxies

When Vajra sits behind a reverse proxy, a load balancer, or a CDN, the
TCP peer address is the *proxy's* address, not the end user's. To recover
the real client you must tell Vajra which peer addresses are proxies:

    [access]
    trusted_proxies = ["127.0.0.1", "10.0.0.0/8"]
    deny_ips        = ["203.0.113.0/24"]

Given a request, Vajra derives the **effective client IP** like this:

1. If the direct peer is **not** in `trusted_proxies`, use the peer IP.
   Forwarding headers are ignored.
2. Otherwise, parse `X-Forwarded-For` right-to-left and pick the first
   entry that is itself **not** a trusted proxy. That is the closest
   address on the chain that Vajra does not control.
3. If `X-Forwarded-For` is absent or yields nothing usable, try
   `X-Real-IP` (single address).
4. If that also fails, fall back to the peer IP.

The trusted-proxy gate is not optional. Without it, any client could
send `X-Forwarded-For: 10.0.0.1` and walk through a `10.0.0.0/8`
allowlist. The gate means: **the header is trusted only if the connection
came from a proxy you configured**.

### Example: nginx on the same host

nginx typically connects from loopback:

    # vajra.toml
    [access]
    allow_ips       = ["10.0.0.0/8"]
    trusted_proxies = ["127.0.0.1", "::1"]

    # nginx.conf
    location / {
        proxy_pass         http://127.0.0.1:8080;
        proxy_set_header   X-Real-IP        $remote_addr;
        proxy_set_header   X-Forwarded-For  $proxy_add_x_forwarded_for;
    }

### Example: Cloudflare

Cloudflare publishes its ranges at <https://www.cloudflare.com/ips/>. Put
the ones you terminate on into `trusted_proxies`. Because Cloudflare
always rewrites `X-Forwarded-For`, the rightmost entry Vajra sees is the
Cloudflare edge that talked to you, and the leftmost is the end user.

    [access]
    trusted_proxies = [
      "173.245.48.0/20", "103.21.244.0/22", "103.22.200.0/22",
      # … full list …
    ]

## Ordering within a request

The access check runs **before** routing, virtual-host matching, static
file serving, and upstream proxying. A denied request never touches the
filesystem, never opens an upstream connection, and never reads the
request body. It gets a bare `403 Forbidden` response.

## Diagnostics

Denials are logged at `info` with both the effective client IP and the
direct peer:

    access: denied client=203.0.113.9 peer=127.0.0.1

If you see a `client` that does not match what you expect, check your
`trusted_proxies` list — a proxy that is missing from it will be reported
as the client, and one that is over-broad will let a spoofed
`X-Forwarded-For` through.

## What this does not do

* No rate limiting, no geo-blocking, no ASN filtering.
* No `Forwarded` (RFC 7239) support — only `X-Forwarded-For` and
  `X-Real-IP`. RFC 7239 is rare in the wild; if you need it, open an
  issue.
* No per-route overrides. `[access]` is global to a site. Per-path
  allow/deny can be added later using the same evaluation core.
* No allow/deny by `Host` header — combine with your proxy layer if you
  need it.

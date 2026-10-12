# `vajra --check`

Validates a configuration file without starting the server. Prints a
structured report and exits non-zero if any **error** is found.

```sh
vajra --check --config /etc/vajra/app.toml
```

## Output

```text
ERRORS:
  [tls] cert /etc/letsencrypt/live/example.com/fullchain.pem: No such file or directory
  [php] proxy[0] socket /run/php/php8.2-fpm.sock: No such file or directory

WARNINGS:
  [tls] cert expires in ~12 day(s)
  [admin] admin endpoint 0.0.0.0:9100 is not loopback - it has NO authentication

INFO:
  [backup] local_dir /var/backups/vajra will be created (parent exists)
  [backup] DB localhost:3306 reachable

OK: 7 check(s), 2 warning(s), 2 error(s)
```

## Checks

| Scope | Severity | Condition |
|---|---|---|
| `[server]` | warning | `listen` port < 1024 but `euid != 0` |
| `[server]` | warning | `workers > 2 x available CPUs` |
| `[server]` | warning | `grace_secs < 2` |
| `[admin]` | warning | admin endpoint is not loopback (no auth) |
| `[admin]` | info | admin endpoint on loopback |
| `[tls]` | error | cert or key file missing / unreadable |
| `[tls]` | error | cert expires in < 7 days |
| `[tls]` | warning | cert expires in < 30 days |
| `[tls]` | info | cert valid for > 90 days (or `openssl` unavailable) |
| `[php]` | error | a PHP `[[proxy]]` references a missing/refusing unix socket |
| `[php]` | error | a TCP upstream is not reachable within 2 s |
| `[php]` | info | socket or upstream reachable |
| `[php]` | info | no `[[proxy]]` routes, or none are PHP |
| `[backup]` | error | `local_dir` is not a directory, or not writable |
| `[backup]` | error | `local_dir` parent does not exist |
| `[backup]` | error | `[backup.database]` host:port not reachable within 2 s |
| `[backup]` | warning | `s3` destination set but no credentials |
| `[backup]` | info | `local_dir` does not exist yet but parent does |

## Exit code

* `0` - no errors (warnings are OK)
* `1` - at least one error

## Design notes

* **Never mutates.** Writes to `local_dir` are probed with a
  `tempfile::NamedTempFile`, deleted immediately.
* **Bounded I/O.** TCP probes use a 2-second `connect_timeout`.
* **No X.509 dependency.** Cert expiry shells out to
  `openssl x509 -checkend`; a missing `openssl` degrades to info.

## See also

* `vajra setup` - generates `/etc/vajra/app.toml` (`docs/setup.md`).

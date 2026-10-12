# Auto-backup

Vajra backs up its own state — primarily the FastCGI cache index and any
operator-declared data roots — to one or more destinations on a schedule.

The **`[backup]` config section describes policy only**. The process that
*executes* a backup is a short-lived subcommand (`vajra backup run`),
fired by a systemd timer or an external cron. This keeps Vajra's worker
threads free of schedulers and matches the `vajra wp` pattern: policy in
config, one-shot executors as subcommands.

---

## Config reference

```toml
[backup]
enabled       = false             # opt-in; other fields ignored when false
schedule      = "0 3 * * *"       # cron expression
keep_days     = 7                 # retention in days, 1..=3650
destinations  = ["local"]         # subset of ["local","s3"]
local_dir     = "/var/backups/vajra"
s3_bucket     = ""                # required iff "s3" in destinations
s3_region     = ""                # required iff "s3" in destinations
s3_access_key = ""                # prefer VAJRA_S3_ACCESS_KEY env
s3_secret_key = ""                # prefer VAJRA_S3_SECRET_KEY env
s3_endpoint   = ""                # optional (MinIO, R2, B2)
s3_prefix     = "vajra/"
```

### Defaults

| Field | Default |
|---|---|
| `enabled` | `false` |
| `schedule` | `"0 3 * * *"` |
| `keep_days` | `7` |
| `destinations` | `[]` |
| `local_dir` | `/var/backups/vajra` |
| `s3_prefix` | `"vajra/"` |

### Validation

1. `enabled = true` + `destinations.is_empty()` → error.
2. `schedule` must parse via the `cron` crate.
3. `keep_days ∈ 1..=3650`.
4. `local_dir` absolute iff `"local"` is a destination.
5. `s3_bucket` + `s3_region` required iff `"s3"` is a destination.
6. Missing S3 creds → warning (instance role may supply them).

Unknown keys inside `[backup]` are rejected (`deny_unknown_fields`).

---

## Schedule

Vajra does not run the schedule itself; it feeds a systemd timer or cron:

```
vajra backup install-timer
# or:
0 3 * * * root /usr/local/bin/vajra backup run >>/var/log/vajra-backup.log 2>&1
```

---

## Destinations

### `local`
- `<local_dir>/vajra-YYYY-MM-DDTHH-MM-SSZ.tar.zst`.
- Written to `<name>.partial`, then `rename(2)`d.
- `local_dir` created on first run with mode `0700`.

### `s3`
- SigV4. Key: `{s3_prefix}{basename}`.
- Credentials resolved: env → config → `~/.aws/credentials` / instance role.
- `s3_endpoint` → path-style (MinIO / R2 / B2).

---

## Retention

After a successful run: list `vajra-*`, read date, delete older than
`keep_days`. Applies **per destination, independently** — a failed S3
upload does not block local cleanup.

---

## Secrets

Credentials are resolved env-first:

| Env var | Overrides |
|---|---|
| `VAJRA_S3_ACCESS_KEY` | `backup.s3_access_key` |
| `VAJRA_S3_SECRET_KEY` | `backup.s3_secret_key` |

Put them in `/etc/vajra/backup.env` (`0600`) and load via systemd
`EnvironmentFile=`.

---

## Failure modes

| Situation | Behaviour |
|---|---|
| Local disk full | Run aborts; existing archives untouched. |
| S3 unreachable | Local write succeeds; upload retried next tick. |
| Partial write | `.partial` orphaned; cleaned up by next successful run. |
| SIGKILL mid-run | Same as partial write. |

Metrics (Day 24+):

- `vajra_backup_runs_total{result="ok|fail",destination="local|s3"}`
- `vajra_backup_last_success_timestamp_seconds`
- `vajra_backup_archive_bytes{destination="..."}`

---

## Restore

```
vajra backup restore <archive> [--into <dir>]
```

Manual and offline. Never automatic.

---

## Non-goals (v1)

- PITR / incremental backups.
- Cross-region replication.
- Encryption at rest beyond TLS-in-transit.
- Arbitrary paths outside Vajra's own state.
- In-process scheduler.

---

## Implementation plan (Day 24+)

1. `src/backup.rs` — `run_once(&BackupConfig) -> Result<BackupReport>`.
2. `src/main.rs` — `vajra backup run|install-timer|restore`.
3. Archive format: `tar` + `zstd`.
4. S3 client: **must be tokio-free**.
5. Metrics wired into the existing Prometheus registry.
6. Integration test with fake local + fake S3 HTTP endpoint.

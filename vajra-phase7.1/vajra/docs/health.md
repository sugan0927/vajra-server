# Health checks

Vajra exposes a dedicated, blocking side-listener for liveness and
readiness probes. It runs outside the io_uring worker pool so probes
succeed even when the main listener is under load.

## Configuration

    [health]
    listen      = "127.0.0.1:9201"
    path        = "/health"
    ready_path  = "/ready"
    enabled     = true
    mariadb     = "127.0.0.1:3306"   # optional

All fields optional. Absent `[health]` means no listener.

PHP-FPM socket is taken from `[php].upstream`.

## Endpoints

| Method | Path     | Behavior                                          |
|--------|----------|---------------------------------------------------|
| GET    | /health  | 200 while the process is alive                    |
| HEAD   | /health  | Same, no body                                     |
| GET    | /ready   | 200 when all deps connect, 503 otherwise          |
| HEAD   | /ready   | Same, no body                                     |
| any    | other    | 404, or 405 for non-GET/HEAD                      |

## Readiness semantics

Uses connect(2), not file-existence:

- unix socket -> UnixStream::connect
- tcp host:port -> TcpStream::connect_timeout(500ms)

## Verify

    curl -fsS  http://127.0.0.1:9201/health
    curl -fsS  http://127.0.0.1:9201/ready
    curl -so /dev/null -w '%{http_code}\n' http://127.0.0.1:9201/nope

## Metrics

Internal atomic counters wired to Prometheus: liveness_ok, readiness_ok,
readiness_fail, not_found, bad_method.

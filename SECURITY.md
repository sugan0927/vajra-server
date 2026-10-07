# Security Policy

## 🔐 Supported Versions

Security updates are provided for the following versions:

| Version | Supported          | Notes                          |
|---------|--------------------|--------------------------------|
| 0.7.x   | ✅ Yes             | Current release                |
| 0.6.x   | ⚠️ Limited         | Security fixes only            |
| < 0.6   | ❌ No              | Please upgrade                 |

## 📢 Reporting a Vulnerability

**Please do NOT report security issues through public GitHub Issues.**

If you discover a security vulnerability in Vajra, report it **privately** using one of the methods below:

### Option 1 — GitHub Security Advisories (Recommended)

1. Go to: https://github.com/sugan0927/vajra-server/security/advisories/new
2. Provide a short title describing the issue
3. Mention affected version(s)
4. Include reproduction steps
5. Submit

### Option 2 — Email

- **To**: sdsugans018@gmail.com
- **Subject**: `[SECURITY] <short-description>`
- **Encryption**: Use PGP if you prefer (public key can be shared on request)

### What to Include in Your Report

A good security report should contain:

- **Description** — What is the vulnerability?
- **Impact** — What harm could result?
- **Reproduction** — Step-by-step instructions to reproduce
- **Affected version(s)** — Which version(s) are vulnerable
- **Proof of concept** — Code, curl command, or screenshot
- **Suggested fix** — Optional, but welcome

## ⏱️ Response Timeline

We aim to meet these timelines:

| Stage | Timeline |
|-------|----------|
| Initial acknowledgment | Within **48 hours** |
| Severity assessment | Within **5 business days** |
| Fix development | **Critical**: 7 days · **High**: 14 days · **Medium/Low**: 30 days |
| Public disclosure | **7 days after** fix release |
| Public credit | With your permission |

### Severity Levels

- 🔴 **Critical** — Remote code execution, authentication bypass, memory corruption
- 🟠 **High** — Denial of service, privilege escalation, information disclosure
- 🟡 **Medium** — Logic flaws, misconfigurations with limited impact
- 🟢 **Low** — Minor issues, hardening suggestions

## 🛡️ Vajra Built-in Security Features

### Architecture Level

- **Memory safety** — Entire codebase is written in Rust, eliminating classic C bugs like buffer overflows and use-after-free
- **Minimal unsafe code** — `unsafe` blocks are minimized
- **Zero-copy parsing** — Fewer memory copies means a smaller attack surface
- **No dynamic dependencies** — Single static binary; no library injection risk

### HTTP Layer

- **Strict HTTP/1.1 parsing** — Built on the `httparse` crate
- **Request smuggling protection** — Conflicting `Content-Length` / `Transfer-Encoding` headers are rejected
- **Header injection prevention** — CRLF injection is blocked
- **Timeout protection** — Mitigates Slowloris-style attacks
- **Body size limits** — Controlled via `max_body_bytes`

### FastCGI / PHP Integration

- **Unix socket IPC** — More secure than TCP (local only)
- **Response size limits** — Malicious PHP output is blocked
- **Parameter injection prevention** — Protection against crafted `PATH_INFO`
- **File access restrictions** — Nothing executes outside `front_controller`

### Process Isolation

- **systemd sandboxing** — `LimitNOFILE`, user limits
- **www-data user** for PHP-FPM — No root privileges
- **Separate worker processes** — Crash isolation

## 🚨 Deployment Best Practices

### 1. Configuration

```toml
# /etc/vajra/app.toml
[server]
listen  = "127.0.0.1:8080"   # ⚠️ NEVER 0.0.0.0 publicly
workers = 1

[limits]
max_body_bytes = 67108864    # 64 MB — adjust per workload

[admin]
listen = "127.0.0.1:9200"    # ⚠️ NEVER expose publicly — no auth

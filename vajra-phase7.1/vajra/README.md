# Vajra Server 🚀

**High-performance Rust web server with Zero-Copy `io_uring` + built-in FastCGI cache**

[![Version](https://img.shields.io/badge/version-0.15.11-blue.svg)](https://github.com/sugan0927/vajra-server/releases)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![Performance](https://img.shields.io/badge/WordPress-4%2C810%20RPS-success.svg)](#-performance-benchmarks)

---

## 📊 Performance Benchmarks

**Verified on Debian 13 (4 vCPU, 4 GB RAM, $5/month VPS)** — October 2026

### Static Files (Zero-Copy `io_uring`)

| Test | File Size | RPS | Latency (p50) | Transfer | Timeouts |
|------|-----------|-----|---------------|----------|----------|
| HTTP/1.1 | 10 KB | **14,105** | 6.7 ms | 145 MB/s | 0 |
| HTTP/1.1 | 1 MB | **411** | 239 ms | **415 MB/s** (3.3 Gbps) | 0 |
| HTTP/2 (m=2) | 10 KB | **6,250** | 14 ms | 64 MB/s | 0 |

### WordPress (FastCGI Cache)

| Test | Concurrency | RPS | Latency (p50) | Timeouts |
|------|-------------|-----|---------------|----------|
| Homepage (cached) | 1 | **4,592** | 238 µs | 0 |
| Homepage (cached) | 10 | **4,792** | 2.4 ms | 0 |
| Homepage (cached) | 50 | **4,810** | 8.7 ms | 0 |
| Homepage (cached) | 100 | **4,795** | 20 ms | 0 |
| `/wp-json/` (dynamic) | 20 | 104 | 121 ms | 0 |

### Security

- ✅ **A+ rating** (testssl.sh) — TLS 1.2, TLS 1.3, QUIC/HTTP3
- ✅ **HTTP/2 + HTTP/3** with 0-RTT session resumption
- ✅ **HSTS preload**, CSP, X-Frame-Options, X-Content-Type-Options

---

## 🏆 Why Vajra?

| Feature | Vajra | Nginx | Apache | LiteSpeed |
|---------|-------|-------|--------|-----------|
| **WordPress (cached)** | **4,810 RPS** | ~2,500 RPS | ~380 RPS | ~5,000 RPS |
| **Static 10 KB** | **14,105 RPS** | ~8,000 RPS | ~3,000 RPS | ~9,000 RPS |
| **Zero-Copy (1 MB)** | ✅ `splice()` | ~`sendfile` | ❌ | ❌ |
| **HTTP/3 (QUIC)** | ✅ Built-in | Compile flag | ❌ | ✅ Paid tier |
| **Built-in FastCGI cache** | ✅ Native | Config + module | ❌ | ✅ Paid tier |
| **Built-in Redis cache** | ✅ Native | Separate service | ❌ | Separate service |
| **License** | Free (MIT) | Free (BSD) | Free (Apache) | Paid |

**Vajra delivers LiteSpeed-class performance on $5 VPS hardware — without a license fee.**

---

## ✨ Features

### Zero-Copy Static File Serving
- **`io_uring` + `splice()`**: Data never enters userspace. Direct disk → NIC.
- **1 MB pipe buffer**: Max kernel throughput per chunk.
- **RAM-based auto-tuning**: Buffer sizes scale with system resources.

### FastCGI Cache (Native)
- **Disk-backed, on-heap**: LRU eviction, size-bounded, TTL-controlled.
- **CMS-aware**: Works with WordPress despite session cookies.
- **Vary handling**: `Accept-Encoding` aware, compression-correct.
- **Zero cookie leakage**: `Set-Cookie` stripped before storing.

### Built-in Redis Object Cache
- **Native Redis 7.0** embedded in Vajra (no separate daemon).
- **Automatic activation** for WordPress via `wp redis enable`.

### HTTP/2 + HTTP/3 + TLS 1.3
- **rustls-based** TLS stack — memory safe, no OpenSSL.
- **QUIC/HTTP3** on UDP/443 with 0-RTT.
- **Automatic certificate renewal** via Let's Encrypt ACME.

### Multi-Distro Support
- ✅ **Debian 12/13, Ubuntu 22.04/24.04** (`apt`)
- ✅ **CentOS Stream 9/10, RHEL, Rocky, AlmaLinux** (`dnf`)
- ✅ **Arch Linux, Manjaro** (`pacman`)
- ✅ **SELinux-aware** on RHEL family
- ✅ **systemd-based** service management

### Production Hardening
- **Auto-tuned kernel** — `somaxconn`, BBR, TCP buffers, `io_uring`.
- **Auto-tuned PHP-FPM** — RAM-aware `pm.max_children`.
- **Auto-tuned Redis** — 25% of total RAM (256 MB – 2 GB).
- **Loopback routing fixed** — no more `cURL error 35` on WordPress.
- **Self-healing scripts** — `vajra-retune`, `vajra-health`, `vajra-warmup`.

---

## 🚀 Quick Install

### One-Line Installer (Debian/Ubuntu/CentOS/RHEL/Arch)

```bash
# Basic install (no site)
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | bash

# Install with WordPress site + SSL
curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | \
  bash -s -- yourdomain.com admin@example.com
What Gets Installed
Vajra binary → /usr/local/bin/vajra

Config → /etc/vajra/app.toml

System service → /etc/systemd/system/vajra.service

FastCGI cache → /var/cache/vajra/fastcgi/ (auto-tuned size)

Redis → Embedded in Vajra (127.0.0.1:6379)

Site CLI → /usr/local/bin/vajra-site

Retune CLI → /usr/local/bin/vajra-retune

Site Management
# Add a new WordPress site (auto SSL via Let's Encrypt)
vajra-site add blog.example.com admin@example.com

# Add a PHP site (no WordPress)
vajra-site add api.example.com admin@example.com --php

# Add a static HTML site
vajra-site add static.example.com admin@example.com --html

# Re-apply system tuning (safe on running production)
sudo vajra-retune
🏗️ Architecture
┌─────────────────────────────────────────────────────────────┐
│                         Client (HTTP/2, HTTP/3)             │
└──────────────────────────┬──────────────────────────────────┘
                           │
                    ┌──────▼──────┐
                    │   Vajra     │  ← 4 Rust workers, io_uring
                    │  (rustls)   │
                    └──┬───────┬──┘
                       │       │
              ┌────────▼─┐  ┌──▼────────────┐
              │  Static  │  │ FastCGI Cache │
              │  Files   │  │  (disk+RAM)   │
              │(Zero-Copy)│  └──┬────────────┘
              └──────────┘     │
                                │  ← cache MISS
                          ┌─────▼──────┐
                          │  PHP-FPM   │  ← auto-tuned workers
                          │(WordPress) │
                          └─────┬──────┘
                                │
                          ┌─────▼──────┐
                          │  MariaDB   │  ← persistent data
                          └────────────┘
                                │
                          ┌─────▼──────┐
                          │   Redis    │  ← object cache (embedded)
                          └────────────┘
🔧 Configuration Example
# /etc/vajra/app.toml

[server]
listen  = "0.0.0.0:443"
workers = 4                 # auto-set to nproc
grace_secs = 5

[logging]
access_log = "/var/log/vajra/access.log"
format = "text"

[tls]
listen = "0.0.0.0:443"
cert   = "/etc/letsencrypt/live/example.com/fullchain.pem"
key    = "/etc/letsencrypt/live/example.com/privkey.pem"

[quic]
listen = "0.0.0.0:443"      # HTTP/3 on UDP/443

[compression]
enabled = true
gzip    = true
gzip_level = 6
brotli  = false             # CPU bottleneck on VPS (see docs)

[object_cache]
enabled = true
listen  = "127.0.0.1:6379"
max_memory_mb = 256         # auto-set to 25% RAM

[fastcgi_cache]
enabled = true
path    = "/var/cache/vajra/fastcgi"
ttl     = 300
max_size_mb = 256
cache_methods = ["GET"]

[[site]]
host     = "example.com"
root     = "/var/www/sites/example.com"
index    = "index.php"
mode     = "wordpress"
php_sock = "unix:/run/php/php8.4-fpm.sock"
tls_cert = "/etc/letsencrypt/live/example.com/fullchain.pem"
tls_key  = "/etc/letsencrypt/live/example.com/privkey.pem"

📈 System Tuning (Auto-Applied)
Installer automatically tunes:

Kernel (/etc/sysctl.d/99-vajra-tuning.conf)
net.core.somaxconn = 65535
net.core.netdev_max_backlog = 65535
net.ipv4.tcp_max_syn_backlog = 65535
net.ipv4.tcp_tw_reuse = 1
net.ipv4.tcp_fin_timeout = 15
net.ipv4.tcp_keepalive_time = 300
fs.file-max = 1000000
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
kernel.io_uring_disabled = 0

systemd (vajra.service.d/limits.conf)
LimitNOFILE=1000000
LimitNPROC=65535


LimitNOFILE=1000000
LimitNPROC=65535
PHP-FPM (RAM-aware pm.max_children)
RAMWorkers
< 1 GB4
1–2 GB8
2–4 GB16
≥ 4 GB32
Redis (25% of RAM, capped)
RAMRedis Size
< 1 GB256 MB
4 GB1 GB
8 GB2 GB
FastCGI Cache (12.5% of RAM, capped)
RAMCache Size
< 8 GB1 GB
16 GB2 GB
≥ 32 GB4 GB

🛡️ Security
A+ TLS — only TLS 1.2 + 1.3 + QUIC (no SSLv2/v3/TLS1.0/1.1)

HSTS preload ready

Content-Security-Policy with upgrade-insecure-requests

X-Frame-Options: SAMEORIGIN

X-Content-Type-Options: nosniff

Automatic Let's Encrypt renewal via certbot timer

rustls — memory-safe TLS stack (no OpenSSL CVEs)

Firewall (UFW / firewalld)
# Automatic via installer
ufw allow 22/tcp       # SSH
ufw allow 80/tcp       # HTTP (ACME)
ufw allow 443/tcp      # HTTPS
ufw allow 443/udp      # HTTP/3 QUIC

🔬 Technical Deep Dive
Zero-Copy Splice Pipeline
┌─────────┐    IORING_OP_SPLICE    ┌──────────┐    IORING_OP_SPLICE    ┌─────────┐
│  Disk   │ ────────────────────▶  │   Pipe   │ ────────────────────▶  │ Socket  │
│ (page   │                        │ (1 MB    │                        │ (TLS)   │
│  cache) │                        │  buffer) │                        │         │
└─────────┘                        └──────────┘                        └─────────┘
         ↑                                                                     ↑
         └─────────── Data NEVER enters userspace (RAM) ────────────────────────┘

Result: 415 MB/s (3.3 Gbps) throughput, ~0% CPU

FastCGI Cache Flow


1. Client requests /blog/post/
2. Vajra checks cache key (method + host + path + query)
3. If HIT:  Serve from disk in <1 ms  ✅
4. If MISS: Forward to PHP-FPM → cache response → serve
5. Cache stores:
   - Set-Cookie STRIPPED (privacy)
   - Vary normalized (Accept-Encoding only)
   - Cache-Control preserved

Cache Key

SHA-256(method || host || path || "?" || query)
Result: /var/cache/vajra/fastcgi/data/<first-2>/<hash>

🐛 Known Limitations
IssueWhyWorkaround
Brotli high CPUReal-time compression blocks Vajra workersKeep brotli = false. Pre-compress at build time if needed.
HTTP/2 c=100+ cliffClient+server same 4-core box = CPU contentionTest from external machine. Real production different IPs.
wp-json no cacheREST API has user-specific responsesExpected. Cache only public pages.
WP-Cron manualDISABLE_WP_CRON=true for reliabilitySystem cron runs every 5 min via installer.
Documentation
BENCHMARKS.md — Detailed benchmark methodology

DOCUMENTATION.md — Architecture and internals

CHANGELOG.md — Version history

GitHub Releases — Binaries
Contributing
Contributions welcome! Please:

Fork the repo

Create feature branch (git checkout -b feat/amazing-feature)

Commit changes (git commit -m 'feat: add amazing feature')

Push branch (git push origin feat/amazing-feature)

Open Pull Request

Development Setup
# Clone
git clone https://github.com/sugan0927/vajra-server.git
cd vajra-server

# Build (Linux x86_64)
cargo build --release --target x86_64-unknown-linux-musl

# Test
cargo test --release

# Benchmark
wrk -t4 -c100 -d10s https://your-test-site/
📜 License
MIT License — see LICENSE for details.

Acknowledgments
Tokio — Async runtime

io-uring crate — Rust io_uring bindings

rustls — Memory-safe TLS

quinn — QUIC implementation

hyper — HTTP library

Let's Encrypt — Free SSL certificates
Comparison Detail (Oct 2026 Benchmarks)
Test: WordPress homepage (cached), 4 vCPU, 4 GB RAM, wrk -t4 -c50 -d10s

ServerVersionRPSLatencyConfig
Vajra + FastCGI cache0.15.114,8108.7 msNative cache
LiteSpeed + LSCache6.3~5,000~10 msPaid license
Nginx + FastCGI cache1.27~2,500~15 msfastcgi_cache module
Nginx + Redis1.27~2,000~20 msphpredis extension
Caddy2.10~940~280 ms (p99)No cache
Apache + mod_php2.4~380~920 ms (p99)No cache
Note: Benchmarks vary by hardware, network, and configuration. Same-box testing introduces ~5-10% variance.
Links
🌐 Website: github.com/sugan0927/vajra-server

📦 Releases: releases

🐛 Issues: issues

💬 Discussions: discussions
⚡ Built with Rust. Fast by design. Zero-Copy by default.

# Vajra Web Server + WordPress Multi-Site Installer

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Debian%20%7C%20Ubuntu%20%7C%20RHEL%20%7C%20Arch-red)]()
[![PHP](https://img.shields.io/badge/PHP-8.5%20%7C%208.4-777BB4?logo=php&logoColor=white)](https://www.php.net/)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

**One VPS. Unlimited WordPress sites.** Install the full stack with a
single command, then create isolated sites with vajra-site add.

    curl -fsSL https://raw.githubusercontent.com/sugan0927/vajra-server/main/install.sh | bash
    vajra-site add blog.example.com you@example.com

Requirements: Linux kernel 5.1+ (io_uring), systemd, x86_64.

## Supported distributions

| Distro | Version | Notes |
|---|---|---|
| **Debian** | 11, 12, 13 | Primary target |
| **Ubuntu** | 20.04, 22.04, 24.04 | Same layout as Debian |
| **RHEL / Rocky / Alma** | 8, 9 | PHP via Remi repo |
| **Fedora** | 38+ | PHP via Remi repo |
| **Arch / Manjaro** | rolling | PHP from main repo |

The installer detects the OS family from /etc/os-release, picks
the right package manager (apt, dnf, or pacman), PHP source
(SURY, Remi, or main repo), and Nginx path layout.

It downloads a statically-linked musl binary from the GitHub
release, so no compilation is needed and glibc version does not
matter. If no tarball exists for the tag, it falls back to
building from source on the target machine.

## Architecture

    Client -> Nginx :443 (TLS) -> Vajra :8080+N -> PHP-FPM -> MariaDB

Each site gets its own:

* /var/www/sites/DOMAIN/ - WordPress root
* wp_DOMAIN - MariaDB database and user
* vajra-site@DOMAIN.service - systemd unit, auto-allocated port
* /etc/nginx/conf.d/DOMAIN.conf - Nginx server block
* Let's Encrypt cert via certbot webroot

## Common commands

    vajra-site add DOMAIN EMAIL
    vajra-site list
    vajra-site info DOMAIN
    vajra-site remove DOMAIN

    systemctl restart vajra-site@DOMAIN.service
    systemctl restart php8.5-fpm

    journalctl -u vajra-site@DOMAIN.service -f
    tail -f /var/log/vajra/DOMAIN.access.log

## Environment variables

| Var | Default | Purpose |
|---|---|---|
| VAJRA_REPO | sugan0927/vajra-server | GitHub repo |
| VAJRA_VERSION | latest | Release tag or latest |
| VAJRA_SKIP_BINARY | 0 | Keep existing /usr/local/bin/vajra |

## Building from source

    git clone https://github.com/sugan0927/vajra-server.git
    cd vajra-server/vajra-phase7.1/vajra
    cargo build --release --locked

    # Portable musl build
    rustup target add x86_64-unknown-linux-musl
    apt install musl-tools
    cargo build --release --locked --target x86_64-unknown-linux-musl

## Troubleshooting

### sudo: command not found during vajra-site add

Minimal installs do not ship sudo. The installer runs as root and
calls WP-CLI directly with --allow-root, so sudo is not needed.

### ERR_TOO_MANY_REDIRECTS on /wp-admin/

Vajra FastCGI bridge does not forward X-Forwarded-* into PHP $_SERVER.
The generated wp-config.php forces $_SERVER['HTTPS'] = 'on'
unconditionally. If you see the loop on an old site, add this just
above the /* That's all, stop editing! */ line:

    $_SERVER['HTTPS']       = 'on';
    $_SERVER['SERVER_PORT'] = 443;
    if (isset($_SERVER['HTTP_X_FORWARDED_HOST'])) {
        $_SERVER['HTTP_HOST'] = $_SERVER['HTTP_X_FORWARDED_HOST'];
    }

## License

MIT - see LICENSE.

## Support

* Issues: https://github.com/sugan0927/vajra-server/issues
* Security: see SECURITY.md

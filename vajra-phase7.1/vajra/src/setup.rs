//! `vajra setup` — interactive configuration wizard.
//!
//! Asks a handful of questions, writes `/etc/vajra/app.toml`, and (in the
//! caller) runs the Day 27 checks on the result.
//!
//! The wizard is driven by a [`Prompter`] trait, so tests can inject
//! scripted answers and never touch `stdin`.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::path::Path;

/// Interface for asking the operator. `StdinPrompter` is the real
/// implementation; `ScriptedPrompter` is used in tests.
pub trait Prompter {
    fn ask(&mut self, prompt: &str, default: Option<&str>) -> io::Result<String>;
    fn ask_yn(&mut self, prompt: &str, default: bool) -> io::Result<bool>;
}

// ─────────────────────────────────────────────────────────────────────
// StdinPrompter
// ─────────────────────────────────────────────────────────────────────

pub struct StdinPrompter;

impl Prompter for StdinPrompter {
    fn ask(&mut self, prompt: &str, default: Option<&str>) -> io::Result<String> {
        let mut out = io::stdout().lock();
        match default {
            Some(d) => write!(out, "{prompt} [{d}]: ")?,
            None => write!(out, "{prompt}: ")?,
        }
        out.flush()?;
        drop(out);
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        let s = line.trim().to_string();
        if s.is_empty() {
            Ok(default.unwrap_or("").into())
        } else {
            Ok(s)
        }
    }

    fn ask_yn(&mut self, prompt: &str, default: bool) -> io::Result<bool> {
        let suffix = if default { "[Y/n]" } else { "[y/N]" };
        loop {
            let raw = self.ask(&format!("{prompt} {suffix}"), None)?;
            let s = raw.trim().to_ascii_lowercase();
            if s.is_empty() {
                return Ok(default);
            }
            match s.as_str() {
                "y" | "yes" | "true" | "1" => return Ok(true),
                "n" | "no" | "false" | "0" => return Ok(false),
                _ => continue,
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// ScriptedPrompter (tests)
// ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ScriptedPrompter {
    lines: VecDeque<String>,
}

impl ScriptedPrompter {
    pub fn new<I, S>(it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            lines: it.into_iter().map(Into::into).collect(),
        }
    }
}

impl Prompter for ScriptedPrompter {
    fn ask(&mut self, _prompt: &str, default: Option<&str>) -> io::Result<String> {
        match self.lines.pop_front() {
            Some(s) if !s.is_empty() => Ok(s),
            _ => Ok(default.unwrap_or("").into()),
        }
    }
    fn ask_yn(&mut self, _prompt: &str, default: bool) -> io::Result<bool> {
        match self.lines.pop_front() {
            Some(s) if s.is_empty() => Ok(default),
            Some(s) => Ok(matches!(
                s.to_ascii_lowercase().as_str(),
                "y" | "yes" | "true" | "1"
            )),
            None => Ok(default),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// The wizard
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Answers {
    pub domain: String,
    pub email: String,
    pub php_version: String,
    pub php_socket: String,
    pub use_redis: bool,
    pub enable_backups: bool,
    pub enable_rate_limit: bool,
    pub rate_rps: u32,
}

/// Run the wizard against `p`, writing the generated TOML to
/// `config_path`. Returns the answers so the caller can log them.
///
/// `overwrite = false` (the default) refuses to clobber an existing
/// config unless the operator confirms.
pub fn run(mut p: impl Prompter, config_path: &Path) -> io::Result<Answers> {
    let domain = {
        let d = p.ask("Domain (e.g. example.com)", None)?;
        if d.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "domain is required",
            ));
        }
        d
    };

    let email = p.ask("Admin email (for future ACME cert issuance)", None)?;

    let detected = detect_php_versions();
    let php_default = detected.last().map(String::as_str);
    let php_hint = if detected.is_empty() {
        "PHP version (e.g. 8.2)".to_string()
    } else {
        format!("PHP version [{}]", detected.join(", "))
    };
    let php_version = p.ask(&php_hint, php_default.or(Some("8.2")))?;
    let php_socket = format!("/run/php/php{php_version}-fpm.sock");

    let use_redis = p.ask_yn("Using Redis for object cache?", false)?;
    let enable_backups = p.ask_yn("Enable auto-backup?", true)?;
    let enable_rate_limit = p.ask_yn("Enable rate limiting?", true)?;
    let rate_rps = if enable_rate_limit {
        let raw = p.ask("Rate limit (requests/sec per IP)", Some("50"))?;
        raw.trim().parse().unwrap_or(50)
    } else {
        0
    };

    let answers = Answers {
        domain,
        email,
        php_version,
        php_socket,
        use_redis,
        enable_backups,
        enable_rate_limit,
        rate_rps,
    };

    if config_path.exists() {
        let yes = p.ask_yn(
            &format!("{} already exists — overwrite?", config_path.display()),
            false,
        )?;
        if !yes {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "aborted by operator",
            ));
        }
    }
    if let Some(parent) = config_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(config_path, render(&answers))?;
    Ok(answers)
}

fn detect_php_versions() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(it) = std::fs::read_dir("/etc/php") {
        for e in it.flatten() {
            let Some(s) = e.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // Match exactly "X.Y" — e.g. "8.1", "8.2", "8.3".
            let bytes = s.as_bytes();
            if bytes.len() == 3
                && bytes[0].is_ascii_digit()
                && bytes[1] == b'.'
                && bytes[2].is_ascii_digit()
            {
                out.push(s);
            }
        }
    }
    out.sort();
    out
}

fn render(a: &Answers) -> String {
    let mut s = String::new();
    s.push_str("# /etc/vajra/app.toml — generated by `vajra setup`\n");
    s.push_str("#\n");
    s.push_str(&format!("# domain:  {}\n", a.domain));
    if a.email.is_empty() {
        s.push_str("# email:   (not set)\n");
    } else {
        s.push_str(&format!(
            "# email:   {} (for future ACME cert issuance)\n",
            a.email
        ));
    }
    s.push_str(&format!(
        "# php:     {} → {}\n",
        a.php_version, a.php_socket
    ));
    s.push_str(&format!(
        "# redis:   {}\n",
        if a.use_redis { "yes" } else { "no" }
    ));
    s.push('\n');

    s.push_str("[server]\n");
    s.push_str("listen     = \"0.0.0.0:80\"\n");
    s.push_str("workers    = 0\n");
    s.push_str("pin        = true\n");
    s.push_str("grace_secs = 5\n\n");

    let cert = format!("/etc/letsencrypt/live/{}/fullchain.pem", a.domain);
    let key = format!("/etc/letsencrypt/live/{}/privkey.pem", a.domain);
    let have_tls = Path::new(&cert).exists() && Path::new(&key).exists();
    if have_tls {
        s.push_str("[tls]\n");
        s.push_str("listen = \"0.0.0.0:443\"\n");
        s.push_str(&format!("cert   = \"{cert}\"\n"));
        s.push_str(&format!("key    = \"{key}\"\n\n"));
    } else {
        s.push_str("# [tls] — uncomment once certs exist (Let's Encrypt):\n");
        s.push_str("# listen = \"0.0.0.0:443\"\n");
        s.push_str(&format!("# cert   = \"{cert}\"\n"));
        s.push_str(&format!("# key    = \"{key}\"\n\n"));
    }

    s.push_str("[static]\n");
    s.push_str("root = \"/var/www/html\"\n\n");

    s.push_str("# FastCGI route to PHP-FPM.\n");
    s.push_str("[[proxy]]\n");
    s.push_str("prefix    = \"/\"\n");
    s.push_str(&format!("upstreams = [\"unix:{}\"]\n", a.php_socket));
    s.push_str("balance   = \"round_robin\"\n\n");

    if a.enable_rate_limit {
        s.push_str("[rate_limit]\n");
        s.push_str("enabled = true\n");
        s.push_str(&format!("rps     = {}\n", a.rate_rps));
        s.push_str("burst   = 100\n\n");
    }

    if a.enable_backups {
        s.push_str("[backup]\n");
        s.push_str("enabled      = true\n");
        s.push_str("schedule     = \"0 0 3 * * *\"\n");
        s.push_str("keep_days    = 7\n");
        s.push_str("destinations = [\"local\"]\n");
        s.push_str("local_dir    = \"/var/backups/vajra\"\n\n");
        s.push_str("[backup.database]\n");
        s.push_str("host = \"localhost\"\n");
        s.push_str("user = \"root\"\n");
        s.push_str("name = \"wordpress\"\n");
    }

    if a.use_redis {
        s.push_str("\n# Redis detected. Vajra does not manage PHP config. Ensure\n");
        s.push_str("# your php.ini has session.save_handler = redis and\n");
        s.push_str("# session.save_path = \"tcp://127.0.0.1:6379\", and the\n");
        s.push_str("# WP Object Cache drop-in is installed.\n");
    }

    s
}

// ─────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_defaults_follow_hints() {
        let mut p = ScriptedPrompter::new(vec!["", "", "", "", "", "", ""]);
        let d = p.ask("domain", Some("example.com")).unwrap();
        assert_eq!(d, "example.com");
        let y = p.ask_yn("backup?", true).unwrap();
        assert!(y);
        let n = p.ask_yn("redis?", false).unwrap();
        assert!(!n);
    }

    #[test]
    fn scripted_explicit_values_override() {
        let mut p = ScriptedPrompter::new(vec!["example.com", "root@example.com"]);
        assert_eq!(p.ask("domain", None).unwrap(), "example.com");
        assert_eq!(p.ask("email", None).unwrap(), "root@example.com");
    }

    #[test]
    fn render_contains_expected_sections() {
        let a = Answers {
            domain: "example.com".into(),
            email: "admin@example.com".into(),
            php_version: "8.2".into(),
            php_socket: "/run/php/php8.2-fpm.sock".into(),
            use_redis: true,
            enable_backups: true,
            enable_rate_limit: true,
            rate_rps: 75,
        };
        let s = render(&a);
        assert!(s.contains("[server]"));
        assert!(s.contains("[static]"));
        assert!(s.contains("[[proxy]]"));
        assert!(s.contains("unix:/run/php/php8.2-fpm.sock"));
        assert!(s.contains("[rate_limit]"));
        assert!(s.contains("rps     = 75"));
        assert!(s.contains("[backup]"));
        assert!(s.contains("[backup.database]"));
        assert!(s.contains("Redis detected"));
    }

    #[test]
    fn render_omits_disabled_sections() {
        let a = Answers {
            domain: "x.test".into(),
            email: "".into(),
            php_version: "8.1".into(),
            php_socket: "/run/php/php8.1-fpm.sock".into(),
            use_redis: false,
            enable_backups: false,
            enable_rate_limit: false,
            rate_rps: 0,
        };
        let s = render(&a);
        assert!(!s.contains("[backup]"));
        assert!(!s.contains("[rate_limit]"));
        assert!(!s.contains("Redis detected"));
    }
}

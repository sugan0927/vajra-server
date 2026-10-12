//! `vajra wp` — run WP-CLI as `www-data`.
//!
//! The subcommand is a thin wrapper: it locates the `wp` binary, drops
//! privileges to `www-data` if invoked as root, and execs it with the
//! caller's arguments. It is intentionally not smarter than that —
//! `wp` already knows how to install plugins, update core, export the
//! database, etc.
//!
//! ## Behaviour
//!
//! * `vajra wp <args...>` forwards `<args...>` to `wp` verbatim.
//! * If `VAJRA_WP_PATH` is set and the caller did not pass `--path`, a
//!   `--path=<VAJRA_WP_PATH>` is injected as the first argument.
//! * If the effective UID is 0, `wp` runs under `sudo -u www-data`.
//!   Otherwise `wp` runs as the current user (which is what you want
//!   when the caller is already `www-data`).
//! * `wp` is resolved via `$PATH` *before* dropping privileges, so a
//!   missing binary gives a readable error instead of a sudo trace.
//!
//! ## Examples
//!
//! ```text
//! vajra wp plugin list
//! vajra wp plugin install woocommerce --activate
//! vajra wp plugin activate woocommerce
//! vajra wp plugin update --all
//! vajra wp core update
//! vajra wp core update-db
//! vajra wp db export /var/backups/site.sql
//! vajra wp --path=/var/www/shop plugin update --all
//! ```
//!
//! Exit code is `wp`'s own; 127 if `wp` is not found; 2 for a usage
//! error (no args at all).

use std::process::Command;

/// Environment variable that overrides wp-cli's `--path` for every
/// `vajra wp ...` invocation.
pub const PATH_ENV: &str = "VAJRA_WP_PATH";

/// System user wp-cli should run as when invoked as root.
pub const WP_USER: &str = "www-data";

/// Entry point. Returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("usage: vajra wp <wp-cli args...>");
        eprintln!("       vajra wp --path=/var/www/site plugin install <slug>");
        return 2;
    }

    // Locate wp. Doing this before sudo means the error is readable.
    let wp = match which("wp") {
        Some(p) => p,
        None => {
            eprintln!("vajra wp: `wp` (WP-CLI) not found on PATH");
            eprintln!("         install it: https://wp-cli.org/");
            return 127;
        }
    };

    let mut argv: Vec<String> = args.to_vec();

    // Inject --path=<VAJRA_WP_PATH> if the caller did not pass one.
    if !has_path_arg(&argv) {
        if let Ok(p) = std::env::var(PATH_ENV) {
            if !p.is_empty() {
                argv.insert(0, format!("--path={p}"));
            }
        }
    }

    let status = if use_sudo() {
        // Drop to www-data. `sudo` does not require a TTY for `-u`.
        let mut c = Command::new("sudo");
        c.arg("-u").arg(WP_USER).arg("--").arg(&wp);
        c.args(&argv);
        c.status()
    } else {
        let mut c = Command::new(&wp);
        c.args(&argv);
        c.status()
    };

    match status {
        Ok(s) => s.code().unwrap_or(128),
        Err(e) => {
            eprintln!("vajra wp: failed to run wp-cli: {e}");
            1
        }
    }
}

fn has_path_arg(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--path" || a.starts_with("--path="))
}

fn is_root() -> bool {
    // Direct syscall avoids spawning `id`.
    unsafe { libc::geteuid() == 0 }
}

/// Whether `vajra wp` should run `wp` under `sudo -u www-data`.
///
/// * `VAJRA_WP_NO_SUDO=1` forces the direct path. Tests and CI use
///   this; it is also a useful escape hatch when the caller wants to
///   manage privileges itself.
/// * Otherwise: only when we are root *and* `sudo` is on `$PATH`.
///   A root process in a container without `sudo` just runs `wp`
///   directly, which is the least surprising fallback.
fn use_sudo() -> bool {
    if std::env::var_os("VAJRA_WP_NO_SUDO").is_some() {
        return false;
    }
    is_root() && which("sudo").is_some()
}

/// Minimal `which`: walk `$PATH`, return the first executable match.
fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if is_executable(&cand) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(md) => md.is_file() && (md.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_path_detects_both_forms() {
        assert!(has_path_arg(&["--path=/x".into(), "plugin".into()]));
        assert!(has_path_arg(&["--path".into(), "/x".into()]));
        assert!(!has_path_arg(&["plugin".into(), "list".into()]));
    }

    #[test]
    fn which_finds_sh_on_path() {
        // `sh` is on every unix CI.
        assert!(which("sh").is_some());
    }

    #[test]
    fn which_returns_none_for_unknown() {
        assert!(which("vajra-definitely-not-a-real-binary").is_none());
    }

    #[test]
    fn empty_args_is_a_usage_error() {
        assert_eq!(run(&[]), 2);
    }
}

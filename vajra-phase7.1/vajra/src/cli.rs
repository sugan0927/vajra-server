//! CLI subcommand dispatcher for Vajra.
//!
//! When invoked as `vajra <subcommand>`, this module extracts the embedded
//! shell orchestrators (init.sh / vajra-site.sh / vajra-harden) into a
//! private temporary directory and execs the target one. When invoked
//! without a recognised subcommand, `main.rs` runs the web server as usual.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

// ── Embedded shell scripts (compiled into the binary) ────────────────
const INIT_SH: &str = include_str!("../shell/init.sh");
const SITE_SH: &str = include_str!("../shell/vajra-site.sh");
const HARDEN_SH: &str = include_str!("../shell/vajra-harden");

// ── Public entry point ───────────────────────────────────────────────
//
// Returns `Some(exit_code)` if a subcommand was handled; `None` to fall
// through to the original `main()` behaviour (i.e. run the web server).
pub fn try_dispatch() -> Option<i32> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        return None;
    }

    let sub = args[1].as_str();
    let rest: Vec<String> = args[2..].to_vec();

    match sub {
        "init" => Some(run_embedded("init.sh", &rest)),

        "site" => Some(run_embedded("vajra-site.sh", &rest)),

        "harden" => Some(run_embedded("vajra-harden", &rest)),

        "rollback" => {
            let mut a = vec!["--rollback".to_string()];
            a.extend(rest);
            Some(run_embedded("vajra-harden", &a))
        }

        "status" => {
            let mut a = rest;
            a.push("--status".to_string());
            Some(run_embedded("vajra-harden", &a))
        }

        "list" | "ls" => Some(run_embedded("vajra-site.sh", &["list".to_string()])),

        "--version" | "-V" => {
            println!("vajra {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }

        "help" => {
            print_cli_help();
            Some(0)
        }

        // Not a subcommand — let main() handle it (running as server,
        // or one of the legacy flags: --config, --listen, --check, ...)
        _ => None,
    }
}

// ── Help text ────────────────────────────────────────────────────────
fn print_cli_help() {
    println!(
        "vajra {} — thread-per-core io_uring web server + site manager

USAGE:
  vajra [serve] [--config FILE] [--listen ADDR] [--admin ADDR]
                [--workers N] [--root DIR] [--no-pin] [--check]
  vajra init
  vajra site add <domain> <email> [--wordpress|--php|--html]
                                   [--hardened] [--fintech] [--skip-ssl]
  vajra site list
  vajra site info <domain>
  vajra site update php <domain> [version]
  vajra site remove <domain> [--keep-files] [--keep-db]
  vajra harden <domain> --profile=<wordpress|fintech>
  vajra rollback <domain>
  vajra status <domain>
  vajra list
  vajra --version
  vajra --help

SUBCOMMANDS:
  init                    Install the Vajra stack (nginx, PHP-FPM,
                          MariaDB, Vajra binary, vajra-site CLI).
  site <action>           Create / list / inspect / remove sites.
  harden <domain>         Apply WordPress or Fintech hardening.
  rollback <domain>       Undo hardening on a site.
  status <domain>         Show hardening status for a site.
  list                    Shortcut for `site list`.

Without a subcommand, `vajra` runs as the web server, reading the
config file passed via --config (or the environment defaults).

Docs: https://github.com/sugan0927/vajra-server",
        env!("CARGO_PKG_VERSION")
    );
}

// ── Embedded-script runner ───────────────────────────────────────────
//
// Extract all three shell scripts into a fresh temporary directory and
// run the requested one with the supplied args. The scripts see a
// consistent `dirname "$0"` so cross-references (e.g. init.sh looking
// for vajra-harden next to itself) resolve correctly.
fn run_embedded(script_name: &str, args: &[String]) -> i32 {
    let tmpdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("❌ vajra: cannot create temp dir: {e}");
            return 1;
        }
    };

    let base: PathBuf = tmpdir.path().to_path_buf();

    // Write all three scripts, executable.
    for (name, body) in [
        ("init.sh", INIT_SH),
        ("vajra-site.sh", SITE_SH),
        ("vajra-harden", HARDEN_SH),
    ] {
        if let Err(e) = write_executable(&base.join(name), body) {
            eprintln!("❌ vajra: cannot write {name}: {e}");
            return 1;
        }
    }

    let target = base.join(script_name);
    let status = Command::new("bash")
        .arg(&target)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("❌ vajra: failed to exec bash {}: {e}", target.display());
            1
        }
    }
    // tmpdir dropped here → auto-deleted
}

fn write_executable(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    fs::write(path, body)?;
    let mut perm = fs::metadata(path)?.permissions();
    perm.set_mode(0o700);
    fs::set_permissions(path, perm)?;
    Ok(())
}

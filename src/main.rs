// herdr-auto-update - check installed herdr plugins for newer upstream commits
// and reinstall them. Runs as a herdr plugin (startup hook + actions) and as a
// standalone CLI.

mod config;
mod registry;
mod updater;

use std::process::ExitCode;

const USAGE: &str = "\
herdr-auto-update - check/update installed herdr plugins

USAGE:
    herdr-auto-update <startup|check|update> [--json] [--config <path>] [--only <plugin_id>]

COMMANDS:
    startup   check and reinstall outdated plugins (used by herdr's startup hook)
    check     report plugins with newer commits upstream; exit 1 if any
    update    check and reinstall all outdated plugins

FLAGS:
    --json           machine-readable output (check/update)
    --config <path>  override the plugin config file
    --only <id>      restrict check/update to one plugin id
    -V, --version    print version
    -h, --help       print help

EXIT CODES:
    0   ok / everything up to date
    1   updates available (check) or one or more reinstalls failed (update/startup)
    2   fatal error, bad usage, or one or more plugin checks errored
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("herdr-auto-update {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    let cmd = args.remove(0);
    let json = take_flag(&mut args, "--json");
    let config_path = match take_value(&mut args, "--config") {
        Ok(v) => v,
        Err(()) => {
            eprintln!("error: --config requires a path argument");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let only = match take_value(&mut args, "--only") {
        Ok(v) => v,
        Err(()) => {
            eprintln!("error: --only requires a plugin id argument");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if !args.is_empty() {
        eprintln!("error: unexpected argument(s): {}", args.join(" "));
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    let cfg = match config::load(config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    match cmd.as_str() {
        "startup" => {
            if only.is_some() {
                eprintln!("error: --only cannot be used with startup");
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            updater::run_startup(&cfg, json)
        }
        "check" => updater::run_check(&cfg, json, only.as_deref()),
        "update" => updater::run_update(&cfg, json, only.as_deref()),
        other => {
            eprintln!("error: unknown command '{other}'");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(i) = args.iter().position(|a| a == flag) {
        args.remove(i);
        true
    } else {
        false
    }
}

/// Remove `--flag <value>` from args. `Err(())` when the flag is present but
/// has no value, or the next argument is another flag (so `--config --json`
/// is not silently interpreted as a config path).
fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, ()> {
    if let Some(i) = args.iter().position(|a| a == flag) {
        args.remove(i);
        if i < args.len() && !args[i].starts_with('-') {
            Ok(Some(args.remove(i)))
        } else {
            Err(())
        }
    } else {
        Ok(None)
    }
}

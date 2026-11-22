//! `kensaku`: headless local search. One binary: CLI, daemon, MCP and HTTP.

mod args;
mod output;
mod run;

use clap::{CommandFactory, Parser};

fn main() {
    let cli = args::Cli::parse();
    restore_sigpipe(&cli.command);

    if let Some(dir) = &cli.data_dir {
        // SAFETY: set before any thread spawns; all later data-dir lookups (and the daemon) read it.
        unsafe { std::env::set_var("KENSAKU_DATA_DIR", dir) };
    }
    if cli.no_daemon {
        // SAFETY: as above, still single-threaded.
        unsafe { std::env::set_var("KENSAKU_NO_DAEMON", "1") };
    }

    if let args::Command::Completions { shell } = &cli.command {
        clap_complete::generate(*shell, &mut args::Cli::command(), "kensaku", &mut std::io::stdout());
        return;
    }

    if let Err(error) = run::execute(&cli) {
        if cli.json {
            let body = serde_json::json!({ "error": error });
            eprintln!("{body}");
        } else {
            eprintln!("kensaku: {}", error.message);
        }
        std::process::exit(error.code.exit_code());
    }
}

/// One-shot commands exit quietly when piped into e.g. `head`, instead of panicking.
/// Long-lived servers keep Rust's default (ignore SIGPIPE) so a dropped client can't kill them.
fn restore_sigpipe(command: &args::Command) {
    #[cfg(unix)]
    if !matches!(
        command,
        args::Command::Daemon(_) | args::Command::Serve { .. } | args::Command::Mcp
    ) {
        // SAFETY: single-threaded at this point; resetting a signal disposition is async-signal-safe.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    #[cfg(not(unix))]
    let _ = command;
}

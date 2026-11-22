//! `kensaku`: headless local search. One binary: CLI, daemon, MCP and HTTP.

mod args;
mod output;
mod run;

use clap::{CommandFactory, Parser};

fn main() {
    let cli = args::Cli::parse();

    if let Some(dir) = &cli.data_dir {
        // SAFETY: set before any thread is spawned; every later lookup of
        // the data dir (and the spawned daemon) reads this variable.
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

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use multilsp::{Config, LspManager};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "multilsp",
    version,
    about = "Several language servers behind one LSP endpoint"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve one LSP client.
    Serve(ServeArgs),
}

#[derive(Args)]
struct ServeArgs {
    /// Speak LSP over stdin and stdout (currently the only transport).
    #[arg(long, required = true)]
    stdio: bool,
    /// Use another executable for a backend, e.g. `--backend clangd=/usr/bin/clangd-18`.
    #[arg(long = "backend", value_name = "NAME=PATH", value_parser = parse_backend)]
    backends: Vec<(String, PathBuf)>,
}

fn parse_backend(arg: &str) -> Result<(String, PathBuf), String> {
    match arg.split_once('=') {
        Some((name, path)) if !name.is_empty() && !path.is_empty() => {
            Ok((name.to_string(), path.into()))
        }
        _ => Err("expected NAME=PATH".to_string()),
    }
}

fn main() {
    let cli = Cli::parse();
    // stdout carries the protocol, so logs go to stderr only.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("MULTILSP_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let Command::Serve(args) = cli.command;
    debug_assert!(args.stdio, "clap requires --stdio");
    let mut config = Config::default();
    for (name, path) in args.backends {
        let Some(spec) = config.backend_mut(&name) else {
            let known: Vec<_> = config.backends.iter().map(|b| b.name.as_str()).collect();
            eprintln!(
                "multilsp: unknown backend '{name}' (known: {})",
                known.join(", ")
            );
            std::process::exit(2);
        };
        spec.set_program(path);
    }

    let runtime = tokio::runtime::Runtime::new().expect("failed to start the async runtime");
    let outcome = runtime.block_on(LspManager::new(config).serve_stdio());
    // Exit directly: dropping the runtime would block on tokio's stdin reader thread until the
    // client closes our stdin, which editors are not required to do.
    std::process::exit(outcome.exit_code());
}

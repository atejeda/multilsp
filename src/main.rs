use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
#[cfg(feature = "embed")]
use multilsp::bundle::{Bundle, data_dir};
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
    /// Inspect or fix the installed copy of the embedded language servers.
    #[command(subcommand)]
    Bundle(BundleCommand),
}

#[derive(Subcommand, Clone, Copy)]
enum BundleCommand {
    /// Show the embedded servers and where they are installed.
    Info,
    /// Hash every installed file against the manifest.
    Verify,
    /// Reinstall from the copy embedded in this binary.
    Repair,
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

    match cli.command {
        Command::Serve(args) => serve(args),
        Command::Bundle(command) => std::process::exit(bundle(command)),
    }
}

fn serve(args: ServeArgs) -> ! {
    debug_assert!(args.stdio, "clap requires --stdio");
    let mut config = Config::default();
    #[cfg(feature = "embed")]
    use_embedded_servers(&mut config);
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

/// Installs the embedded servers on first use and points the config at them. On failure the
/// servers on PATH are used instead.
#[cfg(feature = "embed")]
fn use_embedded_servers(config: &mut Config) {
    let bundle = Bundle::embedded();
    if bundle.is_empty() {
        return;
    }
    match data_dir().and_then(|dir| bundle.install(&dir)) {
        Ok(installation) => config.use_installation(&installation),
        Err(e) => tracing::error!("cannot install the embedded language servers, using PATH: {e}"),
    }
}

#[cfg(feature = "embed")]
fn bundle(command: BundleCommand) -> i32 {
    let bundle = Bundle::embedded();
    if bundle.is_empty() {
        eprintln!("multilsp: this build embeds no language servers for its target");
        return 1;
    }
    let dir = match data_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("multilsp: cannot locate the data directory: {e}");
            return 1;
        }
    };
    match command {
        BundleCommand::Info => {
            let manifest = bundle.manifest();
            println!("bundle    {} ({})", bundle.id(), manifest.target);
            println!("location  {}", bundle.install_dir(&dir).display());
            for backend in &manifest.backends {
                println!(
                    "{:<9} {} [{}] {}",
                    backend.name, backend.version, backend.license, backend.source
                );
            }
            0
        }
        BundleCommand::Verify => match bundle.verify(&dir) {
            Ok(problems) if problems.is_empty() => {
                println!("ok: {}", bundle.install_dir(&dir).display());
                0
            }
            Ok(problems) => {
                for problem in problems {
                    println!("{problem}");
                }
                println!("run `multilsp bundle repair` to reinstall");
                1
            }
            Err(e) => {
                eprintln!("multilsp: {e}");
                1
            }
        },
        BundleCommand::Repair => match bundle.repair(&dir) {
            Ok(installation) => {
                println!("reinstalled: {}", installation.root.display());
                0
            }
            Err(e) => {
                eprintln!("multilsp: repair failed: {e}");
                1
            }
        },
    }
}

#[cfg(not(feature = "embed"))]
fn bundle(_: BundleCommand) -> i32 {
    eprintln!("multilsp: built without embedded language servers (the `embed` feature)");
    1
}

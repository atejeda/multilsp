//! `multilsp` presents several language servers to an editor as one LSP server.
//!
//! The editor talks to [`LspManager::serve`]; each document is routed to the backend that handles
//! its language, and requests that are not about a document go to every running backend with
//! their results combined.
//!
//! ```no_run
//! use multilsp::{Config, LspManager};
//!
//! # async fn run() {
//! let outcome = LspManager::new(Config::default()).serve_stdio().await;
//! std::process::exit(outcome.exit_code());
//! # }
//! ```

mod backend;
mod capabilities;
pub mod config;
mod gateway;
pub mod jsonrpc;
mod router;

pub use config::{BackendSpec, Config, Connection, Connector, Launch};
pub use gateway::Outcome;
use tokio::io::{AsyncRead, AsyncWrite};

pub struct LspManager {
    config: Config,
}

impl LspManager {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Runs one LSP session over the given streams until the client exits or disconnects.
    /// Backends are started on `initialize` and stopped before this returns.
    pub async fn serve<R, W>(&self, reader: R, writer: W) -> Outcome
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        gateway::serve(self.config.clone(), reader, writer).await
    }

    /// [`serve`](Self::serve) over this process's stdin and stdout.
    pub async fn serve_stdio(&self) -> Outcome {
        self.serve(tokio::io::stdin(), tokio::io::stdout()).await
    }
}

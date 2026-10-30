//! Backend registry: which language servers exist, how to launch them, and which documents they own.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone)]
pub struct Config {
    /// Backends in priority order: when several claim a document, the first one wins.
    pub backends: Vec<BackendSpec>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backends: vec![BackendSpec::gopls()],
        }
    }
}

impl Config {
    pub fn backend_mut(&mut self, name: &str) -> Option<&mut BackendSpec> {
        self.backends.iter_mut().find(|b| b.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct BackendSpec {
    pub name: String,
    pub launch: Launch,
    /// LSP `languageId`s sent in `textDocument/didOpen`.
    pub language_ids: Vec<String>,
    /// File extensions without the dot, used when no `languageId` matches.
    pub extensions: Vec<String>,
    /// Exact file names such as `go.mod`, checked before extensions.
    pub file_names: Vec<String>,
}

impl BackendSpec {
    pub fn new(name: impl Into<String>, launch: Launch) -> Self {
        Self {
            name: name.into(),
            launch,
            language_ids: Vec::new(),
            extensions: Vec::new(),
            file_names: Vec::new(),
        }
    }

    pub fn gopls() -> Self {
        let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
        Self {
            // Both VS Code and Neovim spellings of the Go language ids.
            language_ids: strings(&["go", "go.mod", "go.work", "gomod", "gowork", "gotmpl"]),
            extensions: strings(&["go"]),
            file_names: strings(&["go.mod", "go.work"]),
            ..Self::new("go", Launch::command("gopls"))
        }
    }

    /// Points the backend at another executable, keeping its arguments and environment.
    pub fn set_program(&mut self, path: impl Into<PathBuf>) {
        match &mut self.launch {
            Launch::Command { program, .. } => *program = path.into(),
            Launch::Connect(_) => self.launch = Launch::command(path),
        }
    }
}

#[derive(Clone)]
pub enum Launch {
    /// Spawn a subprocess and speak LSP over its stdin and stdout.
    Command {
        program: PathBuf,
        args: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
    },
    /// Obtain an already connected stream, e.g. to a server running in-process.
    Connect(Connector),
}

impl Launch {
    pub fn command(program: impl Into<PathBuf>) -> Self {
        Launch::Command {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }
}

impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Launch::Command { program, args, env } => f
                .debug_struct("Command")
                .field("program", program)
                .field("args", args)
                .field("env", env)
                .finish(),
            Launch::Connect(_) => f.write_str("Connect(..)"),
        }
    }
}

pub type Connector = Arc<dyn Fn() -> io::Result<Connection> + Send + Sync>;

pub struct Connection {
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    pub writer: Box<dyn AsyncWrite + Send + Unpin>,
}

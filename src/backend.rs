//! One running language-server backend: launch, message pumps, and teardown.

use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::config::{BackendSpec, Connection, Launch};
use crate::jsonrpc::{self, Message, MessageReader};

pub(crate) enum BackendEvent {
    Message(Message),
    /// The backend's output stream ended; it will send nothing more.
    Closed,
}

pub(crate) struct Backend {
    name: String,
    outgoing: mpsc::UnboundedSender<Message>,
    child: Option<Child>,
}

impl Backend {
    /// Launches the backend. Its messages arrive on `events` tagged with `index`, followed by
    /// [`BackendEvent::Closed`] once its output ends.
    pub(crate) fn start(
        spec: &BackendSpec,
        index: usize,
        events: mpsc::UnboundedSender<(usize, BackendEvent)>,
    ) -> io::Result<Self> {
        let (connection, child) = match &spec.launch {
            Launch::Command { program, args, env } => {
                let mut child = Command::new(program)
                    .args(args)
                    .envs(env.iter().map(|(k, v)| (k, v)))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| {
                        io::Error::new(e.kind(), format!("cannot run {}: {e}", program.display()))
                    })?;
                let stdin = child.stdin.take().expect("stdin is piped");
                let stdout = child.stdout.take().expect("stdout is piped");
                let stderr = child.stderr.take().expect("stderr is piped");
                tokio::spawn(log_stderr(spec.name.clone(), stderr));
                (
                    Connection {
                        reader: Box::new(stdout),
                        writer: Box::new(stdin),
                    },
                    Some(child),
                )
            }
            Launch::Connect(connect) => (connect()?, None),
        };
        let (outgoing, queue) = mpsc::unbounded_channel();
        tokio::spawn(write_loop(spec.name.clone(), connection.writer, queue));
        tokio::spawn(read_loop(
            spec.name.clone(),
            index,
            connection.reader,
            events,
        ));
        Ok(Self {
            name: spec.name.clone(),
            outgoing,
            child,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Queues a message. If the backend is gone the message is dropped; the reader reports
    /// [`BackendEvent::Closed`] separately.
    pub(crate) fn send(&self, message: Message) {
        let _ = self.outgoing.send(message);
    }

    /// Closes the backend's input after queued messages are written, waits up to `grace` for the
    /// process to exit, then kills it.
    pub(crate) async fn terminate(self, grace: Duration) -> Option<ExitStatus> {
        let Backend {
            name,
            outgoing,
            child,
        } = self;
        drop(outgoing);
        let mut child = child?;
        match tokio::time::timeout(grace, child.wait()).await {
            Ok(Ok(status)) => Some(status),
            Ok(Err(e)) => {
                warn!(backend = %name, "waiting for exit failed: {e}");
                None
            }
            Err(_) => {
                warn!(backend = %name, "did not exit within {grace:?}, killing it");
                let _ = child.start_kill();
                child.wait().await.ok()
            }
        }
    }
}

async fn write_loop(
    name: String,
    mut writer: Box<dyn AsyncWrite + Send + Unpin>,
    mut queue: mpsc::UnboundedReceiver<Message>,
) {
    while let Some(message) = queue.recv().await {
        if let Err(e) = jsonrpc::write_message(&mut writer, message).await {
            debug!(backend = %name, "write failed: {e}");
            return;
        }
    }
    let _ = writer.shutdown().await;
}

async fn read_loop(
    name: String,
    index: usize,
    reader: Box<dyn AsyncRead + Send + Unpin>,
    events: mpsc::UnboundedSender<(usize, BackendEvent)>,
) {
    let mut reader = MessageReader::new(reader);
    loop {
        match reader.read().await {
            Ok(Some(Ok(message))) => {
                if events
                    .send((index, BackendEvent::Message(message)))
                    .is_err()
                {
                    return;
                }
            }
            Ok(Some(Err(e))) => warn!(backend = %name, "ignoring invalid message: {e}"),
            Ok(None) => break,
            Err(e) => {
                warn!(backend = %name, "read failed: {e}");
                break;
            }
        }
    }
    let _ = events.send((index, BackendEvent::Closed));
}

async fn log_stderr(name: String, stderr: ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        debug!(backend = %name, "{line}");
    }
}

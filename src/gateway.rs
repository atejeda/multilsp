//! The client-facing LSP session: lifecycle, routing, and id translation between the client and
//! the backends.
//!
//! Everything runs on one task that owns all state; I/O happens in separate tasks connected by
//! channels, so client output is never interleaved and no locks are needed.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::backend::{Backend, BackendEvent};
use crate::config::{BackendSpec, Config};
use crate::jsonrpc::{
    self, Id, Message, MessageReader, Notification, Request, Response, ResponseError, error_codes,
};
use crate::router;

/// How long `shutdown` waits for backends before answering the client anyway.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a backend gets to exit after `exit` before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

const MESSAGE_TYPE_ERROR: u8 = 1;
const MESSAGE_TYPE_WARNING: u8 = 2;

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The client sent `shutdown` and then `exit`.
    Exited,
    /// The client sent `exit` without a preceding `shutdown`.
    ExitedWithoutShutdown,
    /// The client closed the connection without sending `exit`.
    ClientClosed,
}

impl Outcome {
    /// The process exit code the LSP specification prescribes for this outcome.
    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::Exited => 0,
            Outcome::ExitedWithoutShutdown | Outcome::ClientClosed => 1,
        }
    }
}

pub(crate) async fn serve<R, W>(config: Config, reader: R, writer: W) -> Outcome
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (client_tx, client_queue) = mpsc::unbounded_channel();
    let writer_task = tokio::spawn(write_client(writer, client_queue));
    let (client_events_tx, mut client_events) = mpsc::unbounded_channel();
    let reader_task = tokio::spawn(read_client(reader, client_events_tx));
    let (backend_events_tx, mut backend_events) = mpsc::unbounded_channel();

    let mut gateway = Gateway::new(config.backends, client_tx, backend_events_tx);
    let outcome = loop {
        let deadline = gateway.shutdown_deadline;
        tokio::select! {
            event = client_events.recv() => match event {
                Some(ClientEvent::Message(message)) => {
                    if let Some(outcome) = gateway.on_client(message) {
                        break outcome;
                    }
                }
                Some(ClientEvent::Invalid(reason)) => {
                    gateway.reply(None, Err(ResponseError::new(error_codes::PARSE_ERROR, reason)));
                }
                Some(ClientEvent::Closed) | None => break Outcome::ClientClosed,
            },
            Some((index, event)) = backend_events.recv() => gateway.on_backend(index, event),
            () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                gateway.on_shutdown_timeout();
            }
        }
    };

    reader_task.abort();
    gateway.stop_backends().await;
    // Dropping the gateway closes the client queue, so the writer drains it and finishes.
    drop(gateway);
    let _ = writer_task.await;
    outcome
}

enum ClientEvent {
    Message(Message),
    Invalid(String),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Uninitialized,
    /// `initialize` was forwarded and the backends have not all answered.
    Initializing,
    Running,
    ShuttingDown,
}

enum GroupKind {
    Initialize,
    Shutdown,
    Forward,
}

/// One client request, sent to one or more backends.
struct Group {
    client_id: Id,
    kind: GroupKind,
    pending: usize,
    results: Vec<(usize, Result<Value, ResponseError>)>,
}

struct Gateway {
    specs: Vec<BackendSpec>,
    /// Running backends, index-aligned with `specs`.
    slots: Vec<Option<Backend>>,
    client: mpsc::UnboundedSender<Message>,
    backend_events: mpsc::UnboundedSender<(usize, BackendEvent)>,
    phase: Phase,
    next_id: i64,
    next_group: u64,
    groups: HashMap<u64, Group>,
    /// Requests sent to backends, keyed by (backend, id we assigned).
    outstanding: HashMap<(usize, i64), u64>,
    /// Client requests in flight, for `$/cancelRequest`.
    client_requests: HashMap<Id, u64>,
    /// Backend-to-client requests, keyed by the id we assigned towards the client.
    server_requests: HashMap<i64, (usize, Id)>,
    /// Open documents and the backend they were routed to.
    documents: HashMap<String, usize>,
    shutdown_deadline: Option<Instant>,
}

impl Gateway {
    fn new(
        specs: Vec<BackendSpec>,
        client: mpsc::UnboundedSender<Message>,
        backend_events: mpsc::UnboundedSender<(usize, BackendEvent)>,
    ) -> Self {
        Self {
            slots: specs.iter().map(|_| None).collect(),
            specs,
            client,
            backend_events,
            phase: Phase::Uninitialized,
            next_id: 0,
            next_group: 0,
            groups: HashMap::new(),
            outstanding: HashMap::new(),
            client_requests: HashMap::new(),
            server_requests: HashMap::new(),
            documents: HashMap::new(),
            shutdown_deadline: None,
        }
    }

    // --- client side ---

    /// Handles one client message; returns the outcome once the session is over.
    fn on_client(&mut self, message: Message) -> Option<Outcome> {
        match message {
            Message::Request(request) => self.on_client_request(request),
            Message::Notification(notification) if notification.method == "exit" => {
                return Some(match self.phase {
                    Phase::ShuttingDown => Outcome::Exited,
                    _ => Outcome::ExitedWithoutShutdown,
                });
            }
            Message::Notification(notification) => self.on_client_notification(notification),
            Message::Response(response) => self.on_client_response(response),
        }
        None
    }

    fn on_client_request(&mut self, request: Request) {
        let reject = |code, message: &str| Err(ResponseError::new(code, message));
        match (self.phase, request.method.as_str()) {
            (Phase::Uninitialized, "initialize") => self.initialize(request),
            (Phase::Uninitialized | Phase::Initializing, _) => self.reply(
                Some(request.id),
                reject(
                    error_codes::SERVER_NOT_INITIALIZED,
                    "server is not initialized",
                ),
            ),
            (Phase::Running, "initialize") => self.reply(
                Some(request.id),
                reject(
                    error_codes::INVALID_REQUEST,
                    "server is already initialized",
                ),
            ),
            (Phase::Running, "shutdown") => self.shutdown(request),
            (Phase::Running, _) => self.forward_request(request),
            (Phase::ShuttingDown, _) => self.reply(
                Some(request.id),
                reject(error_codes::INVALID_REQUEST, "server is shutting down"),
            ),
        }
    }

    fn initialize(&mut self, request: Request) {
        let mut params = request.params.unwrap_or_else(|| json!({}));
        if let Some(params) = params.as_object_mut() {
            // Backends exit when this process disappears; their parent is us, not the editor.
            params.insert("processId".into(), json!(std::process::id()));
        }

        let mut failures = Vec::new();
        for index in 0..self.specs.len() {
            let spec = &self.specs[index];
            match Backend::start(spec, index, self.backend_events.clone()) {
                Ok(backend) => {
                    info!(backend = %spec.name, "started");
                    self.slots[index] = Some(backend);
                }
                Err(e) => {
                    warn!(backend = %spec.name, "failed to start: {e}");
                    failures.push(format!("{}: {e}", spec.name));
                    self.slots[index] = None;
                }
            }
        }

        let targets = self.running_indices();
        if targets.is_empty() {
            let message = if failures.is_empty() {
                "no language server backends are configured".to_string()
            } else {
                format!(
                    "no language server backend could be started ({})",
                    failures.join("; ")
                )
            };
            self.reply(
                Some(request.id),
                Err(ResponseError::new(error_codes::INTERNAL_ERROR, message)),
            );
            return;
        }
        for failure in failures {
            self.show_message(MESSAGE_TYPE_WARNING, format!("multilsp: {failure}"));
        }
        self.phase = Phase::Initializing;
        self.dispatch(
            request.id,
            GroupKind::Initialize,
            "initialize",
            Some(params),
            &targets,
        );
    }

    fn shutdown(&mut self, request: Request) {
        self.phase = Phase::ShuttingDown;
        let targets = self.running_indices();
        if targets.is_empty() {
            self.reply(Some(request.id), Ok(Value::Null));
            return;
        }
        self.shutdown_deadline = Some(Instant::now() + SHUTDOWN_TIMEOUT);
        self.dispatch(request.id, GroupKind::Shutdown, "shutdown", None, &targets);
    }

    fn forward_request(&mut self, request: Request) {
        let targets = match router::document_uri(request.params.as_ref()) {
            Some(uri) => match self.owner(uri, None) {
                // No backend handles this kind of document; null is a valid "nothing here".
                None => return self.reply(Some(request.id), Ok(Value::Null)),
                Some(index) if self.running(index).is_none() => {
                    let message = format!(
                        "language server '{}' is not running",
                        self.specs[index].name
                    );
                    let error = ResponseError::new(error_codes::INTERNAL_ERROR, message);
                    return self.reply(Some(request.id), Err(error));
                }
                Some(index) => vec![index],
            },
            None => self.running_indices(),
        };
        if targets.is_empty() {
            let error =
                ResponseError::new(error_codes::INTERNAL_ERROR, "no language server is running");
            return self.reply(Some(request.id), Err(error));
        }
        self.dispatch(
            request.id,
            GroupKind::Forward,
            &request.method,
            request.params,
            &targets,
        );
    }

    fn on_client_notification(&mut self, notification: Notification) {
        if matches!(self.phase, Phase::Uninitialized | Phase::Initializing) {
            debug!(
                "dropping {} received before initialization",
                notification.method
            );
            return;
        }
        match notification.method.as_str() {
            "$/cancelRequest" => self.cancel(notification.params),
            "textDocument/didOpen" => {
                let document = notification
                    .params
                    .as_ref()
                    .and_then(|p| p.get("textDocument"));
                let Some(uri) = document.and_then(|d| d.get("uri")).and_then(Value::as_str) else {
                    warn!("didOpen without a document uri");
                    return;
                };
                let language_id = document
                    .and_then(|d| d.get("languageId"))
                    .and_then(Value::as_str);
                let Some(index) = router::select_backend(&self.specs, uri, language_id) else {
                    debug!("no backend for {uri}");
                    return;
                };
                self.documents.insert(uri.to_string(), index);
                self.send_to(index, Message::Notification(notification));
            }
            _ => match router::document_uri(notification.params.as_ref()).map(str::to_owned) {
                Some(uri) => {
                    let owner = self.owner(&uri, None);
                    if notification.method == "textDocument/didClose" {
                        self.documents.remove(&uri);
                    }
                    if let Some(index) = owner {
                        self.send_to(index, Message::Notification(notification));
                    }
                }
                None => {
                    for index in self.running_indices() {
                        self.send_to(index, Message::Notification(notification.clone()));
                    }
                }
            },
        }
    }

    fn cancel(&mut self, params: Option<Value>) {
        let Some(id) = params
            .and_then(|p| p.get("id").cloned())
            .and_then(|id| serde_json::from_value::<Id>(id).ok())
        else {
            return;
        };
        let Some(&group) = self.client_requests.get(&id) else {
            return;
        };
        let targets: Vec<(usize, i64)> = self
            .outstanding
            .iter()
            .filter(|(_, g)| **g == group)
            .map(|(key, _)| *key)
            .collect();
        for (index, backend_id) in targets {
            self.send_to(
                index,
                Message::notification("$/cancelRequest", Some(json!({"id": backend_id}))),
            );
        }
    }

    /// A client answer to a request a backend sent.
    fn on_client_response(&mut self, response: Response) {
        let Some(Id::Number(id)) = response.id else {
            warn!("ignoring client response with id {:?}", response.id);
            return;
        };
        let Some((index, original)) = self.server_requests.remove(&id) else {
            debug!("ignoring client response to unknown request {id}");
            return;
        };
        self.send_to(
            index,
            Message::Response(Response {
                id: Some(original),
                result: response.result,
            }),
        );
    }

    // --- backend side ---

    fn on_backend(&mut self, index: usize, event: BackendEvent) {
        match event {
            BackendEvent::Message(Message::Response(response)) => {
                let group = match response.id {
                    Some(Id::Number(id)) => self.outstanding.remove(&(index, id)),
                    _ => None,
                };
                match group {
                    Some(group) => self.record(group, index, response.result),
                    None => {
                        debug!(backend = %self.specs[index].name, "ignoring response {:?}", response.id)
                    }
                }
            }
            BackendEvent::Message(Message::Request(request)) => {
                let id = self.next_id();
                self.server_requests.insert(id, (index, request.id));
                self.send_client(Message::Request(Request {
                    id: Id::Number(id),
                    ..request
                }));
            }
            BackendEvent::Message(Message::Notification(notification)) => {
                if notification.method == "$/cancelRequest" {
                    self.forward_backend_cancel(index, notification.params);
                } else {
                    self.send_client(Message::Notification(notification));
                }
            }
            BackendEvent::Closed => self.backend_closed(index),
        }
    }

    fn forward_backend_cancel(&mut self, index: usize, params: Option<Value>) {
        let Some(original) = params
            .and_then(|p| p.get("id").cloned())
            .and_then(|id| serde_json::from_value::<Id>(id).ok())
        else {
            return;
        };
        let ours = self
            .server_requests
            .iter()
            .find(|(_, (i, id))| *i == index && *id == original);
        if let Some((&id, _)) = ours {
            self.send_client(Message::notification(
                "$/cancelRequest",
                Some(json!({"id": id})),
            ));
        }
    }

    fn backend_closed(&mut self, index: usize) {
        let name = self.specs[index].name.clone();
        if let Some(backend) = self.slots[index].take() {
            if self.phase != Phase::ShuttingDown {
                error!(backend = %name, "exited unexpectedly");
                self.show_message(
                    MESSAGE_TYPE_ERROR,
                    format!("multilsp: language server '{name}' exited unexpectedly"),
                );
            }
            tokio::spawn(async move {
                let name = backend.name().to_string();
                if let Some(status) = backend.terminate(EXIT_GRACE).await {
                    info!(backend = %name, "exited with {status}");
                }
            });
        }

        let error = ResponseError::new(
            error_codes::INTERNAL_ERROR,
            format!("language server '{name}' exited"),
        );
        let lost: Vec<(usize, i64)> = self
            .outstanding
            .keys()
            .filter(|(i, _)| *i == index)
            .copied()
            .collect();
        for key in lost {
            if let Some(group) = self.outstanding.remove(&key) {
                self.record(group, index, Err(error.clone()));
            }
        }
        self.server_requests.retain(|_, (i, _)| *i != index);
    }

    fn on_shutdown_timeout(&mut self) {
        self.shutdown_deadline = None;
        let pending: Vec<u64> = self
            .groups
            .iter()
            .filter(|(_, group)| matches!(group.kind, GroupKind::Shutdown))
            .map(|(id, _)| *id)
            .collect();
        for group_id in pending {
            warn!("language servers did not answer shutdown within {SHUTDOWN_TIMEOUT:?}");
            self.outstanding.retain(|_, g| *g != group_id);
            if let Some(group) = self.groups.remove(&group_id) {
                self.complete(group_id, group);
            }
        }
    }

    /// Sends `exit` to every running backend and waits for them to go away.
    async fn stop_backends(&mut self) {
        let mut stopping = Vec::new();
        for slot in &mut self.slots {
            if let Some(backend) = slot.take() {
                backend.send(Message::notification("exit", None));
                stopping.push(tokio::spawn(backend.terminate(EXIT_GRACE)));
            }
        }
        for task in stopping {
            let _ = task.await;
        }
    }

    // --- request groups ---

    fn dispatch(
        &mut self,
        client_id: Id,
        kind: GroupKind,
        method: &str,
        params: Option<Value>,
        targets: &[usize],
    ) {
        let group = self.next_group;
        self.next_group += 1;
        for &index in targets {
            let id = self.next_id();
            self.outstanding.insert((index, id), group);
            let request = Request {
                id: Id::Number(id),
                method: method.to_string(),
                params: params.clone(),
            };
            self.send_to(index, Message::Request(request));
        }
        self.client_requests.insert(client_id.clone(), group);
        self.groups.insert(
            group,
            Group {
                client_id,
                kind,
                pending: targets.len(),
                results: Vec::new(),
            },
        );
    }

    fn record(&mut self, group_id: u64, index: usize, result: Result<Value, ResponseError>) {
        let Some(group) = self.groups.get_mut(&group_id) else {
            return;
        };
        group.results.push((index, result));
        group.pending -= 1;
        if group.pending == 0 {
            let group = self.groups.remove(&group_id).expect("group exists");
            self.complete(group_id, group);
        }
    }

    fn complete(&mut self, group_id: u64, group: Group) {
        if self.client_requests.get(&group.client_id) == Some(&group_id) {
            self.client_requests.remove(&group.client_id);
        }
        match group.kind {
            GroupKind::Forward => {
                let result = merge_results(group.results.into_iter().map(|(_, r)| r).collect());
                self.reply(Some(group.client_id), result);
            }
            GroupKind::Shutdown => self.reply(Some(group.client_id), Ok(Value::Null)),
            GroupKind::Initialize => self.finish_initialize(group.client_id, group.results),
        }
    }

    fn finish_initialize(
        &mut self,
        client_id: Id,
        mut results: Vec<(usize, Result<Value, ResponseError>)>,
    ) {
        results.sort_by_key(|(index, _)| *index);
        let mut capabilities = Vec::new();
        let mut first_error = None;
        for (index, result) in results {
            match result {
                Ok(value) => capabilities.push(
                    value
                        .get("capabilities")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                ),
                Err(e) => {
                    let name = self.specs[index].name.clone();
                    warn!(backend = %name, "initialize failed: {}", e.message);
                    self.show_message(
                        MESSAGE_TYPE_WARNING,
                        format!(
                            "multilsp: language server '{name}' failed to initialize: {}",
                            e.message
                        ),
                    );
                    if let Some(backend) = self.slots[index].take() {
                        backend.send(Message::notification("exit", None));
                        tokio::spawn(backend.terminate(EXIT_GRACE));
                    }
                    first_error.get_or_insert(e);
                }
            }
        }
        if capabilities.is_empty() {
            self.phase = Phase::Uninitialized;
            let error = first_error.unwrap_or_else(|| {
                ResponseError::new(
                    error_codes::INTERNAL_ERROR,
                    "no language server initialized",
                )
            });
            return self.reply(Some(client_id), Err(error));
        }
        self.phase = Phase::Running;
        let result = json!({
            "capabilities": merge_capabilities(capabilities),
            "serverInfo": {"name": "multilsp", "version": env!("CARGO_PKG_VERSION")},
        });
        self.reply(Some(client_id), Ok(result));
    }

    // --- helpers ---

    fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    fn running(&self, index: usize) -> Option<&Backend> {
        self.slots[index].as_ref()
    }

    fn running_indices(&self) -> Vec<usize> {
        (0..self.slots.len())
            .filter(|&i| self.running(i).is_some())
            .collect()
    }

    /// The backend for a document: where it was opened, else where it would be routed.
    fn owner(&self, uri: &str, language_id: Option<&str>) -> Option<usize> {
        self.documents
            .get(uri)
            .copied()
            .or_else(|| router::select_backend(&self.specs, uri, language_id))
    }

    fn send_to(&self, index: usize, message: Message) {
        if let Some(backend) = self.running(index) {
            backend.send(message);
        }
    }

    fn send_client(&self, message: Message) {
        let _ = self.client.send(message);
    }

    fn reply(&self, id: Option<Id>, result: Result<Value, ResponseError>) {
        self.send_client(Message::Response(Response { id, result }));
    }

    fn show_message(&self, kind: u8, message: String) {
        let params = json!({"type": kind, "message": message});
        self.send_client(Message::notification("window/showMessage", Some(params)));
    }
}

/// Combines one request's results from several backends: arrays are concatenated, otherwise the
/// first non-null result wins. Errors are returned only if no backend produced a result.
fn merge_results(results: Vec<Result<Value, ResponseError>>) -> Result<Value, ResponseError> {
    if results.len() == 1 {
        return results.into_iter().next().expect("one result");
    }
    let mut merged: Option<Value> = None;
    let mut first_error = None;
    for result in results {
        match (result, &mut merged) {
            (Err(e), _) => {
                first_error.get_or_insert(e);
            }
            (Ok(Value::Null), _) => {}
            (Ok(Value::Array(items)), Some(Value::Array(acc))) => acc.extend(items),
            (Ok(value), None) => merged = Some(value),
            (Ok(_), Some(_)) => {}
        }
    }
    match (merged, first_error) {
        (Some(value), _) => Ok(value),
        (None, Some(error)) => Err(error),
        (None, None) => Ok(Value::Null),
    }
}

/// Union of the backends' top-level capabilities, earlier backends winning on conflicts.
/// Per-feature reconciliation (spec FR-007) replaces this once several backends run together.
fn merge_capabilities(all: Vec<Value>) -> Value {
    let mut merged = serde_json::Map::new();
    for capabilities in all {
        if let Value::Object(capabilities) = capabilities {
            for (key, value) in capabilities {
                merged.entry(key).or_insert(value);
            }
        }
    }
    Value::Object(merged)
}

async fn read_client<R: AsyncRead + Unpin>(reader: R, events: mpsc::UnboundedSender<ClientEvent>) {
    let mut reader = MessageReader::new(reader);
    loop {
        let event = match reader.read().await {
            Ok(Some(Ok(message))) => ClientEvent::Message(message),
            Ok(Some(Err(e))) => ClientEvent::Invalid(e.to_string()),
            Ok(None) => ClientEvent::Closed,
            Err(e) => {
                error!("reading from client failed: {e}");
                ClientEvent::Closed
            }
        };
        let closed = matches!(event, ClientEvent::Closed);
        if events.send(event).is_err() || closed {
            return;
        }
    }
}

async fn write_client<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut queue: mpsc::UnboundedReceiver<Message>,
) {
    while let Some(message) = queue.recv().await {
        if let Err(e) = jsonrpc::write_message(&mut writer, message).await {
            error!("writing to client failed: {e}");
            return;
        }
    }
    let _ = writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_results() {
        let error = || Err(ResponseError::new(error_codes::INTERNAL_ERROR, "x"));
        assert_eq!(merge_results(vec![error()]), error());
        assert_eq!(
            merge_results(vec![Ok(json!([1])), Ok(Value::Null), Ok(json!([2, 3]))]),
            Ok(json!([1, 2, 3]))
        );
        assert_eq!(
            merge_results(vec![
                Ok(Value::Null),
                Ok(json!({"a": 1})),
                Ok(json!({"b": 2}))
            ]),
            Ok(json!({"a": 1}))
        );
        assert_eq!(merge_results(vec![error(), Ok(json!([1]))]), Ok(json!([1])));
        assert_eq!(merge_results(vec![error(), Ok(Value::Null)]), error());
        assert_eq!(
            merge_results(vec![Ok(Value::Null), Ok(Value::Null)]),
            Ok(Value::Null)
        );
    }

    #[test]
    fn merges_capabilities_first_wins() {
        let merged = merge_capabilities(vec![
            json!({"hoverProvider": true, "textDocumentSync": 2}),
            json!({"hoverProvider": false, "definitionProvider": true}),
        ]);
        assert_eq!(
            merged,
            json!({"hoverProvider": true, "textDocumentSync": 2, "definitionProvider": true})
        );
    }
}

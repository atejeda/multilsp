//! Gateway behavior against in-process fake backends.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::TestClient;
use multilsp::jsonrpc::{
    Id, Message, MessageReader, Request, Response, ResponseError, error_codes, write_message,
};
use multilsp::{BackendSpec, Config, Connection, Launch, LspManager, Outcome};
use serde_json::{Value, json};
use tokio::io::{DuplexStream, ReadHalf, WriteHalf, duplex, split};
use tokio::task::JoinHandle;

/// A backend named `name` that owns files ending in `.{extension}`.
fn fake(name: &str, extension: &str) -> BackendSpec {
    let server = name.to_string();
    let connect = Arc::new(move || {
        let (ours, theirs) = duplex(1 << 16);
        tokio::spawn(run_fake(server.clone(), theirs));
        let (reader, writer) = split(ours);
        Ok(Connection {
            reader: Box::new(reader),
            writer: Box::new(writer),
        })
    });
    let mut spec = BackendSpec::new(name, Launch::Connect(connect));
    spec.extensions = vec![extension.to_string()];
    spec
}

async fn run_fake(name: String, stream: DuplexStream) {
    let (reader, mut writer) = split(stream);
    let mut reader = MessageReader::new(reader);
    let mut slow: Option<Id> = None;
    let ok = |id: Id, result: Value| {
        Message::Response(Response {
            id: Some(id),
            result: Ok(result),
        })
    };
    let err = |id: Id, code: i64| {
        Message::Response(Response {
            id: Some(id),
            result: Err(ResponseError::new(code, "fake")),
        })
    };

    while let Ok(Some(Ok(message))) = reader.read().await {
        let reply = match message {
            Message::Request(request) => match request.method.as_str() {
                "initialize" => {
                    let legend = match name.as_str() {
                        "alpha" => {
                            json!({"tokenTypes": ["a1", "shared"], "tokenModifiers": ["m1"]})
                        }
                        _ => {
                            json!({"tokenTypes": ["shared", "b1"], "tokenModifiers": ["m2", "m1"]})
                        }
                    };
                    let capabilities = json!({
                        "hoverProvider": true,
                        "workspaceSymbolProvider": true,
                        "executeCommandProvider": {"commands": [format!("{name}.run")]},
                        "semanticTokensProvider": {"legend": legend, "full": true},
                    });
                    Some(ok(request.id, json!({ "capabilities": capabilities })))
                }
                "textDocument/hover" => {
                    let uri = request.params.unwrap()["textDocument"]["uri"]
                        .as_str()
                        .unwrap()
                        .to_string();
                    Some(ok(
                        request.id,
                        json!({"contents": format!("{name} hover {uri}")}),
                    ))
                }
                "workspace/symbol" => Some(ok(request.id, json!([{"name": name}]))),
                "workspace/executeCommand" => Some(ok(request.id, json!({"ran": name}))),
                // Type 1 with modifier bit 0, in this backend's own legend.
                "textDocument/semanticTokens/full" => {
                    Some(ok(request.id, json!({"data": [0, 0, 1, 1, 1]})))
                }
                // Not advertised; only served after `fake/register`.
                "textDocument/definition" => Some(ok(request.id, json!({"backend": name}))),
                "fake/slow" => {
                    slow = Some(request.id);
                    None
                }
                "fake/crash" => return,
                "shutdown" => Some(ok(request.id, Value::Null)),
                _ => Some(err(request.id, error_codes::METHOD_NOT_FOUND)),
            },
            Message::Notification(notification) => match notification.method.as_str() {
                "initialized" => Some(Message::Request(Request {
                    id: Id::String(format!("{name}-config")),
                    method: "workspace/configuration".into(),
                    params: Some(json!({"items": [{"section": name}]})),
                })),
                "textDocument/didOpen" => Some(Message::notification(
                    "textDocument/publishDiagnostics",
                    Some(
                        json!({"uri": notification.params.unwrap()["textDocument"]["uri"], "diagnostics": []}),
                    ),
                )),
                // Only honored when it names the id this backend was given.
                "$/cancelRequest" => match slow.take() {
                    Some(id)
                        if notification.params.unwrap()["id"]
                            == serde_json::to_value(&id).unwrap() =>
                    {
                        Some(err(id, error_codes::REQUEST_CANCELLED))
                    }
                    other => {
                        slow = other;
                        None
                    }
                },
                "fake/register" => {
                    let registration = Message::Request(Request {
                        id: Id::String(format!("{name}-register")),
                        method: "client/registerCapability".into(),
                        params: Some(json!({"registrations": [
                            {"id": "r1", "method": "textDocument/definition"}
                        ]})),
                    });
                    if write_message(&mut writer, registration).await.is_err() {
                        return;
                    }
                    Some(Message::notification("fake/registered", None))
                }
                "exit" => return,
                _ => None,
            },
            Message::Response(response)
                if response.id == Some(Id::String(format!("{name}-config"))) =>
            {
                Some(Message::notification(
                    "fake/configured",
                    Some(
                        json!({"backend": name, "id": response.id, "result": response.result.unwrap()}),
                    ),
                ))
            }
            Message::Response(_) => None,
        };
        if let Some(reply) = reply
            && write_message(&mut writer, reply).await.is_err()
        {
            return;
        }
    }
}

struct Session {
    client: TestClient<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>,
    server: JoinHandle<Outcome>,
}

fn start(backends: Vec<BackendSpec>) -> Session {
    let (client_io, server_io) = duplex(1 << 16);
    let (server_reader, server_writer) = split(server_io);
    let manager = LspManager::new(Config { backends });
    let server = tokio::spawn(async move { manager.serve(server_reader, server_writer).await });
    let (reader, writer) = split(client_io);
    Session {
        client: TestClient::new(reader, writer, Duration::from_secs(5)),
        server,
    }
}

impl Session {
    async fn initialize(&mut self) -> Value {
        let params = json!({"processId": null, "rootUri": null, "capabilities": {}});
        let id = self.client.request("initialize", params).await;
        let result = self
            .client
            .response(&id)
            .await
            .result
            .expect("initialize succeeds");
        self.client.notify("initialized", json!({})).await;
        result
    }

    async fn hover(&mut self, uri: &str) -> Result<Value, ResponseError> {
        let params = json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}});
        let id = self.client.request("textDocument/hover", params).await;
        self.client.response(&id).await.result
    }

    async fn open(&mut self, uri: &str) {
        let document =
            json!({"textDocument": {"uri": uri, "languageId": "plain", "version": 1, "text": ""}});
        self.client.notify("textDocument/didOpen", document).await;
    }

    async fn finish(mut self) -> Outcome {
        let id = self.client.request("shutdown", Value::Null).await;
        assert_eq!(self.client.response(&id).await.result, Ok(Value::Null));
        self.client.notify("exit", Value::Null).await;
        self.server.await.unwrap()
    }
}

#[tokio::test]
async fn routes_documents_and_merges_workspace_requests() {
    let mut s = start(vec![fake("alpha", "a"), fake("beta", "b")]);
    let result = s.initialize().await;
    assert_eq!(result["capabilities"]["hoverProvider"], true);
    assert_eq!(result["serverInfo"]["name"], "multilsp");

    // Each backend's workspace/configuration request reaches the client, and the answer goes back
    // to that backend under the id it used.
    let mut configured = Vec::new();
    for _ in 0..2 {
        let params = s
            .client
            .notification("fake/configured")
            .await
            .params
            .unwrap();
        assert_eq!(params["result"], json!([null]));
        let backend = params["backend"].as_str().unwrap().to_string();
        assert_eq!(params["id"], format!("{backend}-config"));
        configured.push(backend);
    }
    configured.sort();
    assert_eq!(configured, ["alpha", "beta"]);

    s.open("file:///w/x.a").await;
    let diagnostics = s
        .client
        .notification("textDocument/publishDiagnostics")
        .await;
    assert_eq!(diagnostics.params.unwrap()["uri"], "file:///w/x.a");

    // The client's own id comes back, whatever its type.
    let id = Id::String("hover-a".into());
    let params =
        json!({"textDocument": {"uri": "file:///w/x.a"}, "position": {"line": 0, "character": 0}});
    s.client
        .request_with_id(id.clone(), "textDocument/hover", params)
        .await;
    let response = s.client.response(&id).await;
    assert_eq!(
        response.result.unwrap()["contents"],
        "alpha hover file:///w/x.a"
    );

    // Not opened: routed by extension.
    assert_eq!(
        s.hover("file:///w/y.b").await.unwrap()["contents"],
        "beta hover file:///w/y.b"
    );
    // No backend for this file type.
    assert_eq!(s.hover("file:///w/z.txt").await, Ok(Value::Null));

    let id = s
        .client
        .request("workspace/symbol", json!({"query": ""}))
        .await;
    let symbols = s.client.response(&id).await.result.unwrap();
    let mut names: Vec<_> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["alpha", "beta"]);

    assert_eq!(s.finish().await, Outcome::Exited);
}

#[tokio::test]
async fn forwards_cancellation() {
    let mut s = start(vec![fake("alpha", "a")]);
    s.initialize().await;
    let id = Id::Number(100);
    s.client
        .request_with_id(id.clone(), "fake/slow", json!({}))
        .await;
    s.client.notify("$/cancelRequest", json!({"id": 100})).await;
    let response = s.client.response(&id).await;
    assert_eq!(
        response.result.unwrap_err().code,
        error_codes::REQUEST_CANCELLED
    );
    assert_eq!(s.finish().await, Outcome::Exited);
}

#[tokio::test]
async fn survives_backend_crash() {
    let mut s = start(vec![fake("alpha", "a"), fake("beta", "b")]);
    s.initialize().await;
    s.open("file:///w/x.a").await;

    let params = json!({"textDocument": {"uri": "file:///w/x.a"}});
    let id = s.client.request("fake/crash", params).await;
    let error = s.client.response(&id).await.result.unwrap_err();
    assert_eq!(error.code, error_codes::INTERNAL_ERROR);
    let shown = s
        .client
        .notification("window/showMessage")
        .await
        .params
        .unwrap();
    assert_eq!(shown["type"], 1);
    assert!(
        shown["message"].as_str().unwrap().contains("'alpha'"),
        "{shown}"
    );

    let error = s.hover("file:///w/x.a").await.unwrap_err();
    assert!(error.message.contains("not running"), "{error:?}");
    // The other backend is unaffected.
    assert_eq!(
        s.hover("file:///w/y.b").await.unwrap()["contents"],
        "beta hover file:///w/y.b"
    );
    assert_eq!(s.finish().await, Outcome::Exited);
}

#[tokio::test]
async fn rejects_requests_before_initialize() {
    let mut s = start(vec![fake("alpha", "a")]);
    let error = s.hover("file:///w/x.a").await.unwrap_err();
    assert_eq!(error.code, error_codes::SERVER_NOT_INITIALIZED);
    s.client.notify("exit", Value::Null).await;
    assert_eq!(s.server.await.unwrap(), Outcome::ExitedWithoutShutdown);
}

#[tokio::test]
async fn initialize_fails_when_no_backend_starts() {
    let mut missing = BackendSpec::new(
        "missing",
        Launch::command("/nonexistent/multilsp-test-server"),
    );
    missing.extensions = vec!["m".into()];
    let mut s = start(vec![missing]);
    let id = s
        .client
        .request("initialize", json!({"capabilities": {}}))
        .await;
    let error = s.client.response(&id).await.result.unwrap_err();
    assert!(error.message.contains("missing"), "{error:?}");
    drop(s.client);
    assert_eq!(s.server.await.unwrap(), Outcome::ClientClosed);
}

#[tokio::test]
async fn ends_when_client_disconnects() {
    let mut s = start(vec![fake("alpha", "a")]);
    s.initialize().await;
    drop(s.client);
    assert_eq!(s.server.await.unwrap(), Outcome::ClientClosed);
}

#[tokio::test]
async fn reconciles_capabilities_across_backends() {
    let mut s = start(vec![fake("alpha", "a"), fake("beta", "b")]);
    let capabilities = s.initialize().await["capabilities"].clone();
    assert_eq!(
        capabilities["executeCommandProvider"]["commands"],
        json!(["alpha.run", "beta.run"])
    );
    assert_eq!(
        capabilities["semanticTokensProvider"]["legend"],
        json!({"tokenTypes": ["a1", "shared", "b1"], "tokenModifiers": ["m1", "m2"]})
    );

    // Commands go to the backend that owns them.
    let id = s
        .client
        .request("workspace/executeCommand", json!({"command": "beta.run"}))
        .await;
    assert_eq!(
        s.client.response(&id).await.result,
        Ok(json!({"ran": "beta"}))
    );

    // Token types and modifiers are translated into the merged legend.
    for (uri, expected) in [
        ("file:///w/x.a", [0, 0, 1, 1, 1]),
        ("file:///w/y.b", [0, 0, 1, 2, 2]),
    ] {
        let params = json!({"textDocument": {"uri": uri}});
        let id = s
            .client
            .request("textDocument/semanticTokens/full", params)
            .await;
        let result = s.client.response(&id).await.result.unwrap();
        assert_eq!(result["data"], json!(expected), "{uri}");
    }
    assert_eq!(s.finish().await, Outcome::Exited);
}

#[tokio::test]
async fn gates_requests_on_backend_capabilities() {
    let mut s = start(vec![fake("alpha", "a")]);
    s.initialize().await;
    let definition =
        json!({"textDocument": {"uri": "file:///w/x.a"}, "position": {"line": 0, "character": 0}});

    // Not advertised: answered by the gateway instead of reaching the backend.
    let id = s
        .client
        .request("textDocument/definition", definition.clone())
        .await;
    assert_eq!(s.client.response(&id).await.result, Ok(Value::Null));

    // Registered dynamically: forwarded.
    s.client.notify("fake/register", json!({})).await;
    s.client.notification("fake/registered").await;
    let id = s
        .client
        .request("textDocument/definition", definition)
        .await;
    assert_eq!(
        s.client.response(&id).await.result,
        Ok(json!({"backend": "alpha"}))
    );
    assert_eq!(s.finish().await, Outcome::Exited);
}

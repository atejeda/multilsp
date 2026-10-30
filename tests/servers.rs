//! End to end through the `multilsp` binary and real language servers. Each test is skipped
//! when its servers are not found; `MULTILSP_GOPLS` and `MULTILSP_CLANGD` point at specific ones.

mod common;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use common::TestClient;
use serde_json::{Value, json};
use tokio::process::{Child, ChildStdin, ChildStdout};

const GO_SOURCE: &str =
    "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n";
const C_SOURCE: &str =
    "int add(int a, int b) { return a + b; }\n\nint main(void) { return add(1, 2); }\n";

fn find(program: &str, env: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(env) {
        return Some(path.into());
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(gopath) = std::env::var_os("GOPATH") {
        dirs.push(PathBuf::from(gopath).join("bin"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("go/bin"));
    }
    dirs.into_iter()
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

struct Server {
    child: Child,
    client: TestClient<ChildStdout, ChildStdin>,
    root_uri: String,
    _dir: tempfile::TempDir,
}

impl Server {
    /// Starts `multilsp` in a temporary workspace holding `files`. `backends` are
    /// `(name, path)` overrides; backends left out run from PATH if installed.
    async fn start(backends: &[(&str, &Path)], files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (name, contents) in files {
            std::fs::write(root.join(name), contents).unwrap();
        }
        let root_uri = format!("file://{}", root.display());

        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_multilsp"));
        command.args(["serve", "--stdio"]);
        for (name, path) in backends {
            command
                .arg("--backend")
                .arg(format!("{name}={}", path.display()));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let client = TestClient::new(stdout, stdin, Duration::from_secs(60));
        Self {
            child,
            client,
            root_uri,
            _dir: dir,
        }
    }

    async fn initialize(&mut self) -> Value {
        let params = json!({
            "processId": std::process::id(),
            "rootUri": self.root_uri,
            "workspaceFolders": [{"uri": self.root_uri, "name": "test"}],
            "capabilities": {
                "general": {"positionEncodings": ["utf-8", "utf-16"]},
                "textDocument": {"semanticTokens": {
                    "requests": {"full": {"delta": true}},
                    "tokenTypes": [], "tokenModifiers": [], "formats": ["relative"],
                }},
            },
        });
        let id = self.client.request("initialize", params).await;
        let result = self
            .client
            .response(&id)
            .await
            .result
            .expect("initialize succeeds");
        self.client.notify("initialized", json!({})).await;
        result["capabilities"].clone()
    }

    async fn open(&mut self, file: &str, language_id: &str, text: &str) -> String {
        let uri = format!("{}/{file}", self.root_uri);
        let document = json!({"textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text}});
        self.client.notify("textDocument/didOpen", document).await;
        uri
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.client.request(method, params).await;
        let result = self.client.response(&id).await.result;
        result.unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
    }

    async fn hover(&mut self, uri: &str, line: u32, character: u32) -> String {
        let params = json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}});
        self.request("textDocument/hover", params).await.to_string()
    }

    async fn finish(mut self) {
        assert_eq!(self.request("shutdown", Value::Null).await, Value::Null);
        self.client.notify("exit", Value::Null).await;
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "{status}");
    }
}

#[tokio::test]
async fn hover_through_gopls() {
    let Some(gopls) = find("gopls", "MULTILSP_GOPLS") else {
        eprintln!("skipping: gopls not found");
        return;
    };
    let files = [
        ("go.mod", "module example.com/hello\n\ngo 1.21\n"),
        ("main.go", GO_SOURCE),
    ];
    let mut server = Server::start(&[("gopls", &gopls)], &files).await;
    assert_ne!(server.initialize().await["hoverProvider"], Value::Null);
    let uri = server.open("main.go", "go", GO_SOURCE).await;
    // Line 5 is `\tfmt.Println("hello")`; character 6 is inside `Println`.
    let hover = server.hover(&uri, 5, 6).await;
    assert!(hover.contains("Println"), "{hover}");
    server.finish().await;
}

#[tokio::test]
async fn hover_through_clangd() {
    let Some(clangd) = find("clangd", "MULTILSP_CLANGD") else {
        eprintln!("skipping: clangd not found");
        return;
    };
    let mut server = Server::start(&[("clangd", &clangd)], &[("main.c", C_SOURCE)]).await;
    assert_ne!(server.initialize().await["hoverProvider"], Value::Null);
    let uri = server.open("main.c", "c", C_SOURCE).await;
    // Line 2 calls `add` at character 24.
    let hover = server.hover(&uri, 2, 25).await;
    assert!(hover.contains("add"), "{hover}");
    server.finish().await;
}

#[tokio::test]
async fn gopls_and_clangd_together() {
    let (Some(gopls), Some(clangd)) = (
        find("gopls", "MULTILSP_GOPLS"),
        find("clangd", "MULTILSP_CLANGD"),
    ) else {
        eprintln!("skipping: needs both gopls and clangd");
        return;
    };
    let files = [
        ("go.mod", "module example.com/hello\n\ngo 1.21\n"),
        ("main.go", GO_SOURCE),
        ("main.c", C_SOURCE),
    ];
    let mut server = Server::start(&[("gopls", &gopls), ("clangd", &clangd)], &files).await;
    let capabilities = server.initialize().await;

    // Both servers' trigger characters and commands are advertised.
    let triggers = capabilities["completionProvider"]["triggerCharacters"].to_string();
    assert!(
        triggers.contains("\".\"") && triggers.contains("\">\""),
        "{triggers}"
    );
    let commands = capabilities["executeCommandProvider"]["commands"].to_string();
    assert!(
        commands.contains("gopls.tidy") && commands.contains("clangd.applyTweak"),
        "{commands}"
    );
    assert!(
        capabilities.get("positionEncoding").is_none(),
        "{capabilities}"
    );

    let go = server.open("main.go", "go", GO_SOURCE).await;
    let c = server.open("main.c", "c", C_SOURCE).await;
    let hover = server.hover(&go, 5, 6).await;
    assert!(hover.contains("Println"), "{hover}");
    let hover = server.hover(&c, 2, 25).await;
    assert!(hover.contains("add"), "{hover}");

    // clangd provides semantic tokens; gopls (by default) does not, so Go gets null, not an error.
    if capabilities["semanticTokensProvider"].is_object() {
        let tokens = server
            .request(
                "textDocument/semanticTokens/full",
                json!({"textDocument": {"uri": c}}),
            )
            .await;
        assert!(!tokens["data"].as_array().unwrap().is_empty(), "{tokens}");
        let tokens = server
            .request(
                "textDocument/semanticTokens/full",
                json!({"textDocument": {"uri": go}}),
            )
            .await;
        assert_eq!(tokens, Value::Null);
    }
    // declaration is clangd-only.
    let position = json!({"line": 5, "character": 6});
    let declaration = server
        .request(
            "textDocument/declaration",
            json!({"textDocument": {"uri": go}, "position": position}),
        )
        .await;
    assert_eq!(declaration, Value::Null);
    server.finish().await;
}

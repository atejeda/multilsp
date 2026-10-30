//! End to end through the `multilsp` binary and a real gopls. Skipped when gopls is not found;
//! set `MULTILSP_GOPLS` to point at a specific one.

mod common;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use common::TestClient;
use serde_json::{Value, json};

fn find_gopls() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MULTILSP_GOPLS") {
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
        .map(|dir| dir.join("gopls"))
        .find(|path| path.is_file())
}

#[tokio::test]
async fn hover_through_gopls() {
    let Some(gopls) = find_gopls() else {
        eprintln!("skipping: gopls not found (set MULTILSP_GOPLS or put gopls on PATH)");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("go.mod"), "module example.com/hello\n\ngo 1.21\n").unwrap();
    let source = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n";
    std::fs::write(root.join("main.go"), source).unwrap();
    let root_uri = format!("file://{}", root.display());
    let file_uri = format!("{root_uri}/main.go");

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_multilsp"))
        .args(["serve", "--stdio", "--backend"])
        .arg(format!("go={}", gopls.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut client = TestClient::new(stdout, stdin, Duration::from_secs(60));

    let params = json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "workspaceFolders": [{"uri": root_uri, "name": "hello"}],
        "capabilities": {},
    });
    let id = client.request("initialize", params).await;
    let result = client
        .response(&id)
        .await
        .result
        .expect("initialize succeeds");
    assert_ne!(
        result["capabilities"]["hoverProvider"],
        Value::Null,
        "{result}"
    );
    client.notify("initialized", json!({})).await;

    let document = json!({"textDocument": {"uri": file_uri, "languageId": "go", "version": 1, "text": source}});
    client.notify("textDocument/didOpen", document).await;

    // Line 5 is `\tfmt.Println("hello")`; character 6 is inside `Println`.
    let params =
        json!({"textDocument": {"uri": file_uri}, "position": {"line": 5, "character": 6}});
    let id = client.request("textDocument/hover", params).await;
    let hover = client.response(&id).await.result.expect("hover succeeds");
    assert!(hover.to_string().contains("Println"), "{hover}");

    let id = client.request("shutdown", Value::Null).await;
    assert_eq!(client.response(&id).await.result, Ok(Value::Null));
    client.notify("exit", Value::Null).await;
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status}");
}

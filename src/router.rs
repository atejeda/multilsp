//! Maps documents to the backend responsible for them.

use serde_json::Value;

use crate::config::BackendSpec;

/// The URI of the document a message is about, if it names one.
pub(crate) fn document_uri(params: Option<&Value>) -> Option<&str> {
    let params = params?;
    params
        .pointer("/textDocument/uri")
        // Call and type hierarchy requests carry the document inside `item`.
        .or_else(|| params.pointer("/item/uri"))
        .and_then(Value::as_str)
}

/// Index of the backend that handles `uri`, preferring an explicit `languageId`.
pub(crate) fn select_backend(
    specs: &[BackendSpec],
    uri: &str,
    language_id: Option<&str>,
) -> Option<usize> {
    if let Some(language_id) = language_id
        && let Some(index) = specs
            .iter()
            .position(|s| s.language_ids.iter().any(|l| l == language_id))
    {
        return Some(index);
    }
    let name = file_name(uri)?;
    if let Some(index) = specs
        .iter()
        .position(|s| s.file_names.iter().any(|f| f == name))
    {
        return Some(index);
    }
    let (_, extension) = name.rsplit_once('.')?;
    specs
        .iter()
        .position(|s| s.extensions.iter().any(|e| e == extension))
}

fn file_name(uri: &str) -> Option<&str> {
    let path = uri.split(['?', '#']).next()?;
    path.rsplit('/').next().filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::Launch;

    fn specs() -> Vec<BackendSpec> {
        let mut c = BackendSpec::new("c", Launch::command("clangd"));
        c.language_ids = vec!["c".into(), "cpp".into()];
        c.extensions = vec!["c".into(), "h".into()];
        vec![BackendSpec::gopls(), c]
    }

    #[test]
    fn prefers_language_id() {
        assert_eq!(
            select_backend(&specs(), "file:///x/main.h", Some("go")),
            Some(0)
        );
        assert_eq!(
            select_backend(&specs(), "file:///x/main.go", Some("cpp")),
            Some(1)
        );
    }

    #[test]
    fn falls_back_to_file_name_then_extension() {
        assert_eq!(select_backend(&specs(), "file:///x/go.mod", None), Some(0));
        assert_eq!(
            select_backend(&specs(), "file:///x/main.go?v=1", Some("unknown")),
            Some(0)
        );
        assert_eq!(select_backend(&specs(), "file:///x/lib.h", None), Some(1));
        assert_eq!(select_backend(&specs(), "file:///x/notes.txt", None), None);
        assert_eq!(select_backend(&specs(), "file:///x/Makefile", None), None);
        assert_eq!(select_backend(&specs(), "file:///x/", None), None);
    }

    #[test]
    fn finds_document_uri() {
        let doc = json!({"textDocument": {"uri": "file:///a.go"}});
        let item = json!({"item": {"uri": "file:///b.go", "name": "f"}});
        assert_eq!(document_uri(Some(&doc)), Some("file:///a.go"));
        assert_eq!(document_uri(Some(&item)), Some("file:///b.go"));
        assert_eq!(document_uri(Some(&json!({"query": ""}))), None);
        assert_eq!(document_uri(None), None);
    }
}

//! Reconciling several backends' capabilities into the single set the client sees, and deciding
//! which backends can serve a request.

use serde_json::{Map, Value, json};

/// Merges the `capabilities` of each backend (index-aligned, `Null` for backends that are not
/// running) into what the client is told, plus a semantic-token remapping per backend.
pub(crate) fn merge(all: &[Value]) -> (Value, Vec<Option<TokenRemap>>) {
    let mut merged = Map::new();
    for capabilities in all {
        if let Some(capabilities) = capabilities.as_object() {
            for (key, value) in capabilities {
                if truthy(Some(value)) {
                    merged.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }
    }

    // Backends are asked for UTF-16 positions, the protocol default (see `initialize_params`).
    merged.remove("positionEncoding");
    merged.insert("textDocumentSync".into(), text_document_sync(all));

    if let Some(Value::Object(completion)) = merged.get_mut("completionProvider") {
        for field in ["triggerCharacters", "allCommitCharacters"] {
            set_union(completion, field, all, "completionProvider");
        }
        let resolve = all
            .iter()
            .any(|c| truthy(c.pointer("/completionProvider/resolveProvider")));
        completion.insert("resolveProvider".into(), resolve.into());
    }
    if let Some(Value::Object(signature)) = merged.get_mut("signatureHelpProvider") {
        for field in ["triggerCharacters", "retriggerCharacters"] {
            set_union(signature, field, all, "signatureHelpProvider");
        }
    }
    if let Some(Value::Object(commands)) = merged.get_mut("executeCommandProvider") {
        set_union(commands, "commands", all, "executeCommandProvider");
    }
    if let Some(Value::Object(on_type)) = merged.get_mut("documentOnTypeFormattingProvider") {
        let first = on_type
            .get("firstTriggerCharacter")
            .cloned()
            .unwrap_or_default();
        let mut more = Vec::new();
        for c in all {
            let provider = c.get("documentOnTypeFormattingProvider");
            let triggers = provider
                .and_then(|p| p.get("firstTriggerCharacter"))
                .into_iter()
                .chain(
                    provider
                        .and_then(|p| p.get("moreTriggerCharacter"))
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                );
            for trigger in triggers {
                if *trigger != first && !more.contains(trigger) {
                    more.push(trigger.clone());
                }
            }
        }
        on_type.insert("moreTriggerCharacter".into(), Value::Array(more));
    }

    let (tokens, remaps) = semantic_tokens(all);
    match tokens {
        Some(provider) => merged.insert("semanticTokensProvider".into(), provider),
        None => merged.remove("semanticTokensProvider"),
    };
    (Value::Object(merged), remaps)
}

/// Removes client capabilities that would let backends pick different position encodings.
pub(crate) fn initialize_params(params: &mut Value) {
    if let Some(general) = params
        .pointer_mut("/capabilities/general")
        .and_then(Value::as_object_mut)
    {
        general.remove("positionEncodings");
    }
    // clangd's pre-LSP-3.17 extension for the same thing.
    if let Some(capabilities) = params
        .get_mut("capabilities")
        .and_then(Value::as_object_mut)
    {
        capabilities.remove("offsetEncoding");
    }
}

/// Whether a backend with these static `capabilities` and dynamically `registered` methods can
/// serve `method`. Methods without a capability flag are assumed supported.
pub(crate) fn supports<'a>(
    capabilities: &Value,
    registered: impl IntoIterator<Item = &'a str>,
    method: &str,
) -> bool {
    if method == "textDocument/willSaveWaitUntil" {
        return truthy(capabilities.pointer("/textDocumentSync/willSaveWaitUntil"));
    }
    let Some(provider) = provider(method) else {
        return true;
    };
    truthy(capabilities.get(provider))
        || registered
            .into_iter()
            .any(|m| self::provider(m) == Some(provider))
}

/// Whether a backend advertises `command` for `workspace/executeCommand`.
pub(crate) fn has_command(capabilities: &Value, command: &str) -> bool {
    strings(capabilities.pointer("/executeCommandProvider/commands")).any(|c| c == command)
}

/// The server capability that gates a request method. Registration methods (e.g.
/// `textDocument/semanticTokens`) map to the same capability as the requests they enable.
fn provider(method: &str) -> Option<&'static str> {
    let provider = match method {
        "textDocument/hover" => "hoverProvider",
        "textDocument/completion" | "completionItem/resolve" => "completionProvider",
        "textDocument/signatureHelp" => "signatureHelpProvider",
        "textDocument/declaration" => "declarationProvider",
        "textDocument/definition" => "definitionProvider",
        "textDocument/typeDefinition" => "typeDefinitionProvider",
        "textDocument/implementation" => "implementationProvider",
        "textDocument/references" => "referencesProvider",
        "textDocument/documentHighlight" => "documentHighlightProvider",
        "textDocument/documentSymbol" => "documentSymbolProvider",
        "textDocument/codeAction" | "codeAction/resolve" => "codeActionProvider",
        "textDocument/codeLens" | "codeLens/resolve" => "codeLensProvider",
        "textDocument/documentLink" | "documentLink/resolve" => "documentLinkProvider",
        "textDocument/documentColor" | "textDocument/colorPresentation" => "colorProvider",
        "textDocument/formatting" => "documentFormattingProvider",
        "textDocument/rangeFormatting" | "textDocument/rangesFormatting" => {
            "documentRangeFormattingProvider"
        }
        "textDocument/onTypeFormatting" => "documentOnTypeFormattingProvider",
        "textDocument/rename" | "textDocument/prepareRename" => "renameProvider",
        "textDocument/foldingRange" => "foldingRangeProvider",
        "textDocument/selectionRange" => "selectionRangeProvider",
        "textDocument/linkedEditingRange" => "linkedEditingRangeProvider",
        "textDocument/prepareCallHierarchy"
        | "callHierarchy/incomingCalls"
        | "callHierarchy/outgoingCalls" => "callHierarchyProvider",
        "textDocument/prepareTypeHierarchy"
        | "typeHierarchy/supertypes"
        | "typeHierarchy/subtypes" => "typeHierarchyProvider",
        "textDocument/moniker" => "monikerProvider",
        "textDocument/inlayHint" | "inlayHint/resolve" => "inlayHintProvider",
        "textDocument/inlineValue" => "inlineValueProvider",
        "textDocument/diagnostic" => "diagnosticProvider",
        "workspace/symbol" | "workspaceSymbol/resolve" => "workspaceSymbolProvider",
        "workspace/executeCommand" => "executeCommandProvider",
        m if m.starts_with("textDocument/semanticTokens") => "semanticTokensProvider",
        _ => return None,
    };
    Some(provider)
}

/// One `textDocumentSync` for all backends. Full sync is used if any backend asks for it: a
/// backend that handles incremental changes also accepts whole-document ones.
fn text_document_sync(all: &[Value]) -> Value {
    let mut change: Option<u64> = None;
    let (mut open_close, mut will_save, mut will_save_wait_until) = (false, false, false);
    let mut save: Option<bool> = None; // Some(include_text)
    for capabilities in all {
        let mut kind = None;
        match capabilities.get("textDocumentSync") {
            Some(Value::Number(n)) => {
                kind = n.as_u64();
                open_close = true;
                save.get_or_insert(false);
            }
            Some(Value::Object(sync)) => {
                kind = sync.get("change").and_then(Value::as_u64);
                open_close |= truthy(sync.get("openClose"));
                will_save |= truthy(sync.get("willSave"));
                will_save_wait_until |= truthy(sync.get("willSaveWaitUntil"));
                match sync.get("save") {
                    Some(Value::Bool(true)) => {
                        save.get_or_insert(false);
                    }
                    Some(Value::Object(options)) => {
                        let include_text = truthy(options.get("includeText"));
                        save = Some(save.unwrap_or(false) || include_text);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if let Some(kind) = kind.filter(|&k| k > 0) {
            change = Some(change.map_or(kind, |c| c.min(kind)));
        }
    }
    let mut sync = json!({
        "openClose": open_close,
        "change": change.unwrap_or(0),
        "willSave": will_save,
        "willSaveWaitUntil": will_save_wait_until,
    });
    if let Some(include_text) = save {
        sync["save"] = json!({"includeText": include_text});
    }
    sync
}

/// Translates one backend's semantic-token indices into the merged legend.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TokenRemap {
    types: Vec<u32>,
    modifiers: Vec<u32>,
}

impl TokenRemap {
    /// Rewrites the `data` of a full or range semantic-tokens result in place.
    pub(crate) fn apply(&self, result: &mut Value) {
        let Some(data) = result.get_mut("data").and_then(Value::as_array_mut) else {
            return;
        };
        for token in data.chunks_mut(5) {
            if let [_, _, _, kind, modifiers] = token {
                if let Some(&mapped) = kind.as_u64().and_then(|k| self.types.get(k as usize)) {
                    *kind = mapped.into();
                }
                if let Some(bits) = modifiers.as_u64() {
                    let mapped = (0..self.modifiers.len())
                        .filter(|&bit| bit < 64 && bits & (1 << bit) != 0)
                        .map(|bit| self.modifiers[bit])
                        .filter(|&target| target < 32)
                        .fold(0u32, |acc, target| acc | 1 << target);
                    *modifiers = mapped.into();
                }
            }
        }
    }
}

/// A combined legend and the remapping for each backend. Deltas are not advertised, because
/// remapping edits to another server's token stream is not worth it.
fn semantic_tokens(all: &[Value]) -> (Option<Value>, Vec<Option<TokenRemap>>) {
    let mut types: Vec<String> = Vec::new();
    let mut modifiers: Vec<String> = Vec::new();
    let (mut full, mut range) = (false, false);
    let remaps = all
        .iter()
        .map(|capabilities| {
            let provider = capabilities
                .get("semanticTokensProvider")
                .filter(|p| truthy(Some(p)))?;
            full |= truthy(provider.get("full"));
            range |= truthy(provider.get("range"));
            Some(TokenRemap {
                types: intern(&mut types, provider.pointer("/legend/tokenTypes")),
                modifiers: intern(&mut modifiers, provider.pointer("/legend/tokenModifiers")),
            })
        })
        .collect::<Vec<_>>();
    if remaps.iter().all(Option::is_none) {
        return (None, remaps);
    }
    let provider = json!({
        "legend": {"tokenTypes": types, "tokenModifiers": modifiers},
        "full": full,
        "range": range,
    });
    (Some(provider), remaps)
}

/// Adds each name to `table` if absent and returns the table index of every name.
fn intern(table: &mut Vec<String>, names: Option<&Value>) -> Vec<u32> {
    strings(names)
        .map(|name| {
            let index = table.iter().position(|t| t == name).unwrap_or_else(|| {
                table.push(name.to_string());
                table.len() - 1
            });
            index as u32
        })
        .collect()
}

/// Sets `object[field]` to the union of `capabilities[provider][field]` across backends.
fn set_union(object: &mut Map<String, Value>, field: &str, all: &[Value], provider: &str) {
    let mut union: Vec<&str> = Vec::new();
    for capabilities in all {
        for item in strings(capabilities.get(provider).and_then(|p| p.get(field))) {
            if !union.contains(&item) {
                union.push(item);
            }
        }
    }
    if !union.is_empty() {
        object.insert(field.into(), json!(union));
    }
}

fn strings(value: Option<&Value>) -> impl Iterator<Item = &str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// A capability counts as present unless it is missing, `null` or `false`.
fn truthy(value: Option<&Value>) -> bool {
    !matches!(value, None | Some(Value::Null) | Some(Value::Bool(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gopls() -> Value {
        json!({
            "textDocumentSync": {"openClose": true, "change": 2, "save": {}},
            "completionProvider": {"triggerCharacters": ["."]},
            "signatureHelpProvider": {"triggerCharacters": ["(", ","], "retriggerCharacters": [")"]},
            "executeCommandProvider": {"commands": ["gopls.tidy"]},
            "codeLensProvider": {},
            "hoverProvider": true,
            "declarationProvider": false,
        })
    }

    fn clangd() -> Value {
        json!({
            "textDocumentSync": {"change": 2, "openClose": true, "save": true},
            "completionProvider": {"resolveProvider": false, "triggerCharacters": [".", "<", ">", ":"]},
            "signatureHelpProvider": {"triggerCharacters": ["(", ")", ","]},
            "executeCommandProvider": {"commands": ["clangd.applyTweak"]},
            "documentOnTypeFormattingProvider": {"firstTriggerCharacter": "\n", "moreTriggerCharacter": []},
            "declarationProvider": true,
            "hoverProvider": true,
            "positionEncoding": "utf-8",
        })
    }

    #[test]
    fn unions_lists_and_keeps_every_provider() {
        let (merged, _) = merge(&[gopls(), clangd()]);
        assert_eq!(
            merged["completionProvider"]["triggerCharacters"],
            json!([".", "<", ">", ":"])
        );
        assert_eq!(merged["completionProvider"]["resolveProvider"], false);
        assert_eq!(
            merged["signatureHelpProvider"]["triggerCharacters"],
            json!(["(", ",", ")"])
        );
        assert_eq!(
            merged["signatureHelpProvider"]["retriggerCharacters"],
            json!([")"])
        );
        assert_eq!(
            merged["executeCommandProvider"]["commands"],
            json!(["gopls.tidy", "clangd.applyTweak"])
        );
        // gopls says false, clangd says true: a false must not hide the other backend's support.
        assert_eq!(merged["declarationProvider"], true);
        assert_eq!(merged["codeLensProvider"], json!({}));
        assert_eq!(
            merged["documentOnTypeFormattingProvider"]["firstTriggerCharacter"],
            "\n"
        );
        assert!(merged.get("positionEncoding").is_none());
        assert_eq!(
            merged["textDocumentSync"],
            json!({"openClose": true, "change": 2, "willSave": false, "willSaveWaitUntil": false,
                   "save": {"includeText": false}})
        );
    }

    #[test]
    fn falls_back_to_full_sync_and_skips_missing_backends() {
        let (merged, _) = merge(&[
            json!({"textDocumentSync": 1}),
            Value::Null,
            json!({"textDocumentSync": {"change": 2}}),
        ]);
        assert_eq!(merged["textDocumentSync"]["change"], 1);
        assert_eq!(merged["textDocumentSync"]["openClose"], true);
    }

    #[test]
    fn checks_support_per_backend() {
        let caps = gopls();
        assert!(supports(&caps, [], "textDocument/hover"));
        assert!(!supports(&caps, [], "textDocument/declaration"));
        assert!(!supports(&caps, [], "textDocument/semanticTokens/full"));
        assert!(supports(
            &caps,
            ["textDocument/semanticTokens"],
            "textDocument/semanticTokens/full"
        ));
        assert!(supports(&caps, [], "custom/method"));
        assert!(has_command(&caps, "gopls.tidy"));
        assert!(!has_command(&caps, "clangd.applyTweak"));
    }

    #[test]
    fn merges_and_remaps_semantic_token_legends() {
        let alpha = json!({"semanticTokensProvider": {
            "legend": {"tokenTypes": ["a1", "shared"], "tokenModifiers": ["m1"]}, "full": {"delta": true}}});
        let beta = json!({"semanticTokensProvider": {
            "legend": {"tokenTypes": ["shared", "b1"], "tokenModifiers": ["m2", "m1"]}, "range": true}});
        let (merged, remaps) = merge(&[alpha, Value::Null, beta]);
        assert_eq!(
            merged["semanticTokensProvider"],
            json!({"legend": {"tokenTypes": ["a1", "shared", "b1"], "tokenModifiers": ["m1", "m2"]},
                   "full": true, "range": true})
        );
        assert!(remaps[1].is_none());

        // beta: one `shared` token with m2+m1, one `b1` token with m2.
        let mut result = json!({"data": [0, 0, 3, 0, 3, 1, 4, 2, 1, 1]});
        remaps[2].as_ref().unwrap().apply(&mut result);
        assert_eq!(result["data"], json!([0, 0, 3, 1, 3, 1, 4, 2, 2, 2]));
    }
}

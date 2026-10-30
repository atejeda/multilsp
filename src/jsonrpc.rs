//! JSON-RPC 2.0 messages and the LSP base-protocol framing (`Content-Length` headers).

use std::fmt;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Upper bound for a single message body, so a corrupt header fails fast instead of allocating.
const MAX_CONTENT_LENGTH: usize = 256 * 1024 * 1024;

/// Error codes defined by JSON-RPC 2.0 and the LSP specification.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INTERNAL_ERROR: i64 = -32603;
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
    pub const REQUEST_CANCELLED: i64 = -32800;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Number(n) => write!(f, "{n}"),
            Id::String(s) => write!(f, "{s:?}"),
        }
    }
}

impl From<Id> for Value {
    fn from(id: Id) -> Self {
        match id {
            Id::Number(n) => n.into(),
            Id::String(s) => s.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: Id,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// `None` only for errors about messages whose id could not be read.
    pub id: Option<Id>,
    pub result: Result<Value, ResponseError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ResponseError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

/// A well-framed body that is not a valid JSON-RPC message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidMessage(pub String);

impl fmt::Display for InvalidMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidMessage {}

impl Message {
    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Message::Notification(Notification {
            method: method.into(),
            params,
        })
    }

    pub fn from_value(value: Value) -> Result<Self, InvalidMessage> {
        let invalid = |reason: String| InvalidMessage(reason);
        let Value::Object(mut obj) = value else {
            return Err(invalid("message is not a JSON object".into()));
        };
        // Outer `None`: no id member; inner `None`: `"id": null`.
        let id = match obj.remove("id") {
            None => None,
            Some(v) => Some(
                serde_json::from_value::<Option<Id>>(v)
                    .map_err(|e| invalid(format!("invalid id: {e}")))?,
            ),
        };
        let params = obj.remove("params");

        if let Some(method) = obj.remove("method") {
            let Value::String(method) = method else {
                return Err(invalid("method is not a string".into()));
            };
            return Ok(match id.flatten() {
                Some(id) => Message::Request(Request { id, method, params }),
                None => Message::Notification(Notification { method, params }),
            });
        }

        let Some(id) = id else {
            return Err(invalid("message has neither a method nor an id".into()));
        };
        let result = if let Some(error) = obj.remove("error") {
            Err(serde_json::from_value(error)
                .map_err(|e| invalid(format!("invalid error: {e}")))?)
        } else if let Some(result) = obj.remove("result") {
            Ok(result)
        } else {
            return Err(invalid("response has neither a result nor an error".into()));
        };
        Ok(Message::Response(Response { id, result }))
    }

    pub fn into_value(self) -> Value {
        let mut obj = Map::new();
        obj.insert("jsonrpc".into(), "2.0".into());
        match self {
            Message::Request(r) => {
                obj.insert("id".into(), r.id.into());
                obj.insert("method".into(), r.method.into());
                if let Some(params) = r.params {
                    obj.insert("params".into(), params);
                }
            }
            Message::Notification(n) => {
                obj.insert("method".into(), n.method.into());
                if let Some(params) = n.params {
                    obj.insert("params".into(), params);
                }
            }
            Message::Response(r) => {
                obj.insert("id".into(), r.id.map_or(Value::Null, Value::from));
                match r.result {
                    Ok(result) => obj.insert("result".into(), result),
                    Err(error) => obj.insert(
                        "error".into(),
                        serde_json::to_value(error).expect("ResponseError serializes"),
                    ),
                };
            }
        }
        Value::Object(obj)
    }
}

/// Reads framed messages from a byte stream.
pub struct MessageReader<R> {
    inner: BufReader<R>,
    line: String,
}

impl<R: AsyncRead + Unpin> MessageReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
            line: String::new(),
        }
    }

    /// Reads the next message.
    ///
    /// Returns `Ok(None)` when the stream ends at a message boundary, `Ok(Some(Err(_)))` for a
    /// well-framed body that is not a valid message (the stream stays usable), and `Err` when the
    /// framing itself is broken.
    pub async fn read(&mut self) -> io::Result<Option<Result<Message, InvalidMessage>>> {
        let mut content_length = None;
        let mut at_boundary = true;
        loop {
            self.line.clear();
            if self.inner.read_line(&mut self.line).await? == 0 {
                if at_boundary {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended inside message headers",
                ));
            }
            at_boundary = false;
            let line = self.line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(invalid_data(format!("malformed header line {line:?}")));
            };
            if name.trim().eq_ignore_ascii_case("content-length") {
                let length = value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| invalid_data(format!("invalid Content-Length {value:?}")))?;
                if length > MAX_CONTENT_LENGTH {
                    return Err(invalid_data(format!(
                        "Content-Length {length} is too large"
                    )));
                }
                content_length = Some(length);
            }
        }
        let length = content_length.ok_or_else(|| invalid_data("missing Content-Length header"))?;
        let mut body = vec![0; length];
        self.inner.read_exact(&mut body).await?;
        Ok(Some(
            serde_json::from_slice::<Value>(&body)
                .map_err(|e| InvalidMessage(format!("invalid JSON: {e}")))
                .and_then(Message::from_value),
        ))
    }
}

/// Writes one framed message and flushes.
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: Message,
) -> io::Result<()> {
    let body = serde_json::to_vec(&message.into_value())?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    async fn read_all(bytes: &[u8]) -> Vec<Message> {
        let mut reader = MessageReader::new(bytes);
        let mut messages = Vec::new();
        while let Some(message) = reader.read().await.unwrap() {
            messages.push(message.unwrap());
        }
        messages
    }

    #[tokio::test]
    async fn round_trips_every_message_kind() {
        let messages = vec![
            Message::Request(Request {
                id: Id::Number(1),
                method: "initialize".into(),
                params: Some(json!({"capabilities": {}})),
            }),
            Message::Request(Request {
                id: Id::String("a".into()),
                method: "shutdown".into(),
                params: None,
            }),
            Message::notification("initialized", Some(json!({}))),
            Message::Response(Response {
                id: Some(Id::Number(1)),
                result: Ok(Value::Null),
            }),
            Message::Response(Response {
                id: None,
                result: Err(ResponseError::new(error_codes::PARSE_ERROR, "bad")),
            }),
        ];
        let mut bytes = Vec::new();
        for message in messages.clone() {
            write_message(&mut bytes, message).await.unwrap();
        }
        assert_eq!(read_all(&bytes).await, messages);
    }

    #[tokio::test]
    async fn accepts_extra_headers_and_bare_newlines() {
        let body = r#"{"jsonrpc":"2.0","method":"exit"}"#;
        let bytes = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\ncontent-length: {}\n\n{body}",
            body.len()
        );
        assert_eq!(
            read_all(bytes.as_bytes()).await,
            vec![Message::notification("exit", None)]
        );
    }

    #[tokio::test]
    async fn invalid_body_keeps_stream_usable() {
        let mut bytes = b"Content-Length: 2\r\n\r\n[]".to_vec();
        write_message(&mut bytes, Message::notification("exit", None))
            .await
            .unwrap();
        let mut reader = MessageReader::new(&bytes[..]);
        assert!(reader.read().await.unwrap().unwrap().is_err());
        assert_eq!(
            reader.read().await.unwrap().unwrap().unwrap(),
            Message::notification("exit", None)
        );
        assert!(reader.read().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn broken_framing_is_an_error() {
        for bytes in [
            &b"Content-Length: 10\r\n"[..],
            b"garbage\r\n\r\n",
            b"\r\n",
            b"Content-Length: 5\r\n\r\n{}",
        ] {
            assert!(MessageReader::new(bytes).read().await.is_err(), "{bytes:?}");
        }
    }
}

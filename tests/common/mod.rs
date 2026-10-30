//! A scripted LSP client for integration tests.

#![allow(dead_code)]

use std::time::Duration;

use multilsp::jsonrpc::{self, Id, Message, MessageReader, Notification, Request, Response};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

pub struct TestClient<R, W> {
    reader: MessageReader<R>,
    writer: W,
    next_id: i64,
    timeout: Duration,
    /// Notifications seen while waiting for something else.
    notifications: Vec<Notification>,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> TestClient<R, W> {
    pub fn new(reader: R, writer: W, timeout: Duration) -> Self {
        Self {
            reader: MessageReader::new(reader),
            writer,
            next_id: 0,
            timeout,
            notifications: Vec::new(),
        }
    }

    pub async fn send(&mut self, message: Message) {
        jsonrpc::write_message(&mut self.writer, message)
            .await
            .expect("write to server");
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Id {
        self.next_id += 1;
        let id = Id::Number(self.next_id);
        self.request_with_id(id.clone(), method, params).await;
        id
    }

    pub async fn request_with_id(&mut self, id: Id, method: &str, params: Value) {
        let params = (!params.is_null()).then_some(params);
        self.send(Message::Request(Request {
            id,
            method: method.into(),
            params,
        }))
        .await;
    }

    pub async fn notify(&mut self, method: &str, params: Value) {
        let params = (!params.is_null()).then_some(params);
        self.send(Message::notification(method, params)).await;
    }

    pub async fn recv(&mut self) -> Message {
        tokio::time::timeout(self.timeout, self.reader.read())
            .await
            .expect("timed out waiting for the server")
            .expect("read from server")
            .expect("server closed the stream")
            .expect("server sent an invalid message")
    }

    /// Waits for the response to `id`, answering server requests and buffering notifications.
    pub async fn response(&mut self, id: &Id) -> Response {
        loop {
            match self.recv().await {
                Message::Response(response) if response.id.as_ref() == Some(id) => return response,
                Message::Response(response) => panic!("unexpected response {response:?}"),
                Message::Request(request) => self.answer(request).await,
                Message::Notification(notification) => self.notifications.push(notification),
            }
        }
    }

    /// Waits for the next notification named `method`, answering server requests meanwhile.
    pub async fn notification(&mut self, method: &str) -> Notification {
        if let Some(i) = self.notifications.iter().position(|n| n.method == method) {
            return self.notifications.remove(i);
        }
        loop {
            match self.recv().await {
                Message::Notification(n) if n.method == method => return n,
                Message::Notification(n) => self.notifications.push(n),
                Message::Request(request) => self.answer(request).await,
                Message::Response(response) => panic!("unexpected response {response:?}"),
            }
        }
    }

    /// Answers a server request the way a minimal client would.
    async fn answer(&mut self, request: Request) {
        let result = match request.method.as_str() {
            "workspace/configuration" => {
                let items = request
                    .params
                    .as_ref()
                    .and_then(|p| p["items"].as_array())
                    .map_or(0, Vec::len);
                Value::Array(vec![Value::Null; items])
            }
            _ => Value::Null,
        };
        self.send(Message::Response(Response {
            id: Some(request.id),
            result: Ok(result),
        }))
        .await;
    }
}

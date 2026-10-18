use std::sync::Arc;

use redj_control::Session;
use redj_core::Library;

#[tokio::test]
async fn http_transport_initializes() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    redj_mcp::serve_http(Arc::new(Session::new(Library::new())), port).unwrap();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let text = resp.text().await.unwrap();
    assert!(text.contains("\"redj\""), "{text}");
}

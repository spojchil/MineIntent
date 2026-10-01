//! Local HTTP boundary test: actual tool result -> pinned framework -> wire image.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent::adapters::http::{HttpModel, HttpModelConfig, Protocol};
use agent::persistence::SessionId;
use agent::{
    InputMessage, Model, ModelOutput, ModelRequest, RunId, ToolCall, ToolResultBatch,
    TranscriptItem,
};
use dispatch::ToolProvider;
use perception::{Picture, PictureDoor, PictureTools};
use serde_json::{json, Value};

struct TestPicture;
impl PictureDoor for TestPicture {
    fn capture<'a>(
        &'a self,
        _region: Option<perception::Region>,
    ) -> agent::PortFuture<'a, Result<Picture, String>> {
        // Payload bytes are deliberately tiny: this tests transport, not PNG decoding.
        Box::pin(async {
            Ok(Picture {
                png: b"\x89PNG\r\n\x1a\n".to_vec(),
                description: "transport fixture".to_owned(),
            })
        })
    }
}

#[tokio::test]
async fn picture_tool_reaches_responses_as_image_with_its_call_id() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("local test connection failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let (header_end, body_len) = loop {
            let mut chunk = [0; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&chunk[..count]);
            assert!(request.len() < 1024 * 1024);
            if let Some(index) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..index]).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (index + 4, length);
            }
        };
        while request.len() < header_end + body_len {
            let mut chunk = [0; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&chunk[..count]);
        }
        let body: Value =
            serde_json::from_slice(&request[header_end..header_end + body_len]).unwrap();
        let response = json!({"id":"test-response", "status":"completed", "output":[{
            "id":"msg-1", "type":"message", "role":"assistant", "status":"completed",
            "content":[{"type":"output_text","text":"received"}]
        }]})
        .to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        body
    });
    let tools = PictureTools::new(Arc::new(TestPicture));
    let call = ToolCall::new("picture-call-1", "view", json!({}));
    let result = tools.call(call.clone()).await;
    let transcript = vec![
        InputMessage::text("user", "view the scene").into(),
        TranscriptItem::ModelOutput(ModelOutput::calls(vec![call])),
        TranscriptItem::ToolResults(ToolResultBatch {
            results: vec![result],
        }),
    ];
    let request = ModelRequest::new(
        SessionId::new("picture-test").unwrap(),
        RunId::new("picture-test/run/1"),
        1,
        transcript,
        tools
            .tools()
            .into_iter()
            .map(|(definition, _)| definition)
            .collect(),
    );
    let model = HttpModel::new(
        HttpModelConfig::new(
            format!("http://{address}/responses"),
            "local-test-only",
            "vision-test",
            Protocol::openai_responses(),
        )
        .with_timeout(Duration::from_secs(5)),
    )
    .unwrap();
    model.complete(request).await.unwrap();
    let body = server.join().unwrap();
    let result = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(result["call_id"], "picture-call-1");
    assert_eq!(result["output"][0]["type"], "input_text");
    assert_eq!(result["output"][1]["type"], "input_image");
    assert_eq!(
        result["output"][1]["image_url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
}

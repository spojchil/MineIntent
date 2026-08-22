//! 模型请求超时探针：对着一个「接受连接但永不回话」的服务端发一次请求，
//! 看适配器多久放弃、以什么错误放弃。
//!
//! 背景：实盘出现过一次模型请求长时间不返回把整跑拖死（issue #134）。
//! 按代码有两层保护——`HttpModelConfig::DEFAULT_TIMEOUT`（60 秒，reqwest 的总
//! 超时）与 engine 的 `model_timeout`（120 秒，`tokio::time::timeout` 包住
//! `complete_stream`）。要在可控条件下能分别验证：
//! **本层（适配器）自己的那道超时到底生不生效？**
//!
//! 本探针只测适配器这一层。engine 那层不在这里（它包在会话循环里）。
//!
//! 用法：
//! ```text
//! # 另开一个终端起假服务端（接受连接后什么都不发）
//! python -c "import socket;s=socket.socket();s.setsockopt(1,2,1);\
//!   s.bind(('127.0.0.1',18080));s.listen(8);\
//!   [print('accepted') for _ in iter(lambda: s.accept(), None)]"
//!
//! cargo run -p companion --example model_timeout_probe -- http://127.0.0.1:18080/v1 5
//! ```
//! 第二个参数是给适配器配的超时秒数（默认 5，省得等满 60）。

use std::time::{Duration, Instant};

use agent::adapters::http::{HttpModel, HttpModelConfig, Protocol};
use agent::persistence::SessionId;
use agent::{
    AgentError, InputMessage, Model, ModelRequest, ModelStreamEvent, ModelStreamSink, PortFuture,
    RunId,
};

/// 什么都不做的 sink：本探针不关心流事件，只关心「多久返回、返回什么」。
struct NullSink;

impl ModelStreamSink for NullSink {
    fn emit<'a>(&'a mut self, _event: ModelStreamEvent) -> PortFuture<'a, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let endpoint = args
        .next()
        .unwrap_or_else(|| "http://127.0.0.1:18080/v1".to_owned());
    let seconds: u64 = args
        .next()
        .unwrap_or_else(|| "5".to_owned())
        .parse()
        .map_err(|error| format!("秒数无效：{error}"))?;

    let model = HttpModel::new(
        HttpModelConfig::new(
            endpoint.clone(),
            "probe-key",
            "probe-model",
            Protocol::openai_chat(),
        )
        .with_timeout(Duration::from_secs(seconds)),
    )
    .map_err(|error| format!("适配器构造失败：{error}"))?;

    println!("[探针] 端点 {endpoint}，配置超时 {seconds}s；开始发请求");
    let request = ModelRequest::new(
        SessionId::new("probe").map_err(|error| format!("会话 id 无效：{error}"))?,
        RunId::new("probe/run/1"),
        1,
        vec![InputMessage::text("user", "hi").into()],
        Vec::new(),
    );

    let started = Instant::now();
    let outcome = model.complete_stream(request, &mut NullSink).await;
    let elapsed = started.elapsed();

    match outcome {
        Ok(_) => println!(
            "[探针] {:.1}s 后拿到了响应（本探针不该走到这里）",
            elapsed.as_secs_f64()
        ),
        Err(error) => println!("[探针] {:.1}s 后放弃：{error}", elapsed.as_secs_f64()),
    }
    println!(
        "[探针] 判读：耗时接近 {seconds}s 说明适配器超时生效；远超则说明这道超时对\
本路径无效——那正是长跑里悬住 8 分钟的解释。"
    );
    Ok(())
}

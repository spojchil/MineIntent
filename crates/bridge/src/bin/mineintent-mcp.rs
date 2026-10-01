//! 标准 MCP stdio 转接器。stdout 专用于协议，日志写 stderr。

use std::time::Duration;

fn main() -> Result<(), String> {
    let addr = std::env::var("MINEINTENT_BODY_ADDR")
        .unwrap_or_else(|_| bridge::protocol::DEFAULT_ADDR.to_owned());
    let shim = bridge::shim::Shim::new(bridge::protocol::parse_addr(&addr)?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("创建 MCP 运行时失败：{error}"))?;
    let result = runtime.block_on(bridge::shim::run(
        tokio::io::stdin(),
        tokio::io::stdout(),
        shim,
    ));
    // Tokio 的 stdout 使用阻塞线程。宿主不读管道时，IO guard 能释放协议和身体，
    // 但已开始的系统写操作不能取消；独立转接进程也不能无限等它才退出。
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

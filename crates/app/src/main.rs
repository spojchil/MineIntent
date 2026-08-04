//! mineintent 可执行入口：加载 env 配置 → 启动 → Ctrl-C 停机。

use mineintent_app::{load_app_config, MineIntentApp};

#[tokio::main]
async fn main() {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("无法取得工作目录：{error}");
            std::process::exit(1);
        }
    };
    let config = match load_app_config(&cwd) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("配置错误：{error}");
            std::process::exit(1);
        }
    };
    println!(
        "MineIntent 启动：server={}:{} world={} data={}",
        config.minecraft.server.host,
        config.minecraft.server.port,
        config.minecraft.world_id,
        config.data_directory.display(),
    );
    let app = match MineIntentApp::start(config).await {
        Ok(app) => app,
        Err(error) => {
            eprintln!("启动失败：{error}");
            std::process::exit(1);
        }
    };
    println!("已就绪；Ctrl-C 停机。");
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("信号监听失败：{error}");
    }
    println!("停机中……");
    match app.stop("app_stopped").await {
        Ok(()) => println!("已停止。"),
        Err(error) => {
            eprintln!("停机存在错误：{error}");
            std::process::exit(1);
        }
    }
}

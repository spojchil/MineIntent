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
    // 有界运行：MINEINTENT_MAX_RUNTIME_SECS 供纵向实测等无人值守场景，
    // 到时视同 Ctrl-C，走同一条正常停机路径（不是 kill）。
    let max_runtime = std::env::var("MINEINTENT_MAX_RUNTIME_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    match max_runtime {
        Some(secs) => {
            println!("已就绪；Ctrl-C 或 {secs}s 后停机。");
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(error) = result {
                        eprintln!("信号监听失败：{error}");
                    }
                }
                () = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {
                    println!("达到 MINEINTENT_MAX_RUNTIME_SECS={secs}，按计划停机。");
                }
            }
        }
        None => {
            println!("已就绪；Ctrl-C 停机。");
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("信号监听失败：{error}");
            }
        }
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

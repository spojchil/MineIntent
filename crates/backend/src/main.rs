use std::{io::BufRead, time::Duration};

use mineintent_backend::runtime::{run_with_handle, RunConfig, RuntimeHandle};

fn main() {
    let config = parse_args();
    let handle = RuntimeHandle::new(config.clone());
    start_command_reader(handle.clone());
    let result = tokio::runtime::Runtime::new()
        .expect("创建 Tokio runtime 失败")
        .block_on(run_with_handle(handle, config));
    if let Err(error) = result {
        eprintln!("后端运行失败：{error}");
        std::process::exit(1);
    }
}

fn start_command_reader(handle: RuntimeHandle) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(command) => {
                    if let Err(error) = handle.send_command(command) {
                        eprintln!("命令未入队：{error}");
                    }
                }
                Err(error) => eprintln!("命令 JSON 无法解析：{error}"),
            }
        }
    });
}

fn parse_args() -> RunConfig {
    let mut config = RunConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| panic!("参数 {arg} 缺少值"));
        match arg.as_str() {
            "--host" => config.host = value(),
            "--port" => config.port = value().parse().expect("--port 必须是数字"),
            "--username" => config.username = value(),
            "--world-id" => config.world_id = value(),
            "--duration-secs" => {
                config.duration =
                    Duration::from_secs(value().parse().expect("--duration-secs 必须是数字"));
            }
            "--reconnect-delay-secs" => {
                config.reconnect_delay = Duration::from_secs(
                    value().parse().expect("--reconnect-delay-secs 必须是数字"),
                );
            }
            "--send-chat" => config.initial_chat = Some(value()),
            "--help" | "-h" => {
                println!("用法：mineintent-backend [--host HOST] [--port PORT] [--username NAME] [--duration-secs N] [--send-chat MESSAGE]；也可从 stdin 逐行输入 BackendCommandEnvelope JSON");
                std::process::exit(0);
            }
            other => panic!("未知参数：{other}"),
        }
    }
    config
}

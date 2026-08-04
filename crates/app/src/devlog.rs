//! 开发者模式（`MINEINTENT_DEBUG=1`）：把运行中真正发生的事落到
//! `<数据目录>/dev.log`，并保留每轮模型请求/响应原文。
//!
//! 判据与 journal 分工：journal 是产品事实的持久记录（有 schema、要迁移）；
//! dev.log 是排障用的过程记录，不承诺格式稳定，默认关闭。出问题时先看它，
//! 而不是去读代码猜。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::Mutex;

struct DevLog {
    file: Mutex<std::fs::File>,
    dir: PathBuf,
}

static DEV_LOG: OnceLock<Option<DevLog>> = OnceLock::new();

/// 是否开启：`MINEINTENT_DEBUG` 为 1/true/on 即开。
pub fn enabled() -> bool {
    matches!(
        std::env::var("MINEINTENT_DEBUG")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// 装配期调用一次；未开启时是空操作。
pub fn init(data_dir: &Path) {
    let _ = DEV_LOG.get_or_init(|| {
        if !enabled() {
            return None;
        }
        let _ = std::fs::create_dir_all(data_dir);
        let path = data_dir.join("dev.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        println!("[dev] 详细日志已开启：{}", path.display());
        Some(DevLog {
            file: Mutex::new(file),
            dir: data_dir.to_path_buf(),
        })
    });
}

fn stamp() -> String {
    // 仅用于排障排序，取本地 UTC 秒级即可；不引入额外时间依赖。
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    format!("{}.{:03}", secs, millis)
}

/// 写一行分类日志；`stage` 用于粗筛（wake/model/tool/failure/lifecycle）。
pub fn log(stage: &str, message: impl AsRef<str>) {
    let Some(Some(dev)) = DEV_LOG.get() else {
        return;
    };
    let line = format!("{} [{}] {}\n", stamp(), stage, message.as_ref());
    print!("[dev] {line}");
    let mut file = dev.file.lock();
    let _ = file.write_all(line.as_bytes());
    let _ = file.flush();
}

/// 保留模型一轮的请求/响应原文，返回是否落盘。
pub fn dump_model_io(tag: &str, kind: &str, body: &str) -> Option<PathBuf> {
    let Some(Some(dev)) = DEV_LOG.get() else {
        return None;
    };
    let dir = dev.dir.join("model-io");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{tag}-{kind}.json"));
    std::fs::write(&path, body).ok()?;
    Some(path)
}

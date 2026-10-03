//! 本地文件放哪：缓存与数据分开，和启动器一样。
//!
//! - **缓存**（删了能重新下载）：客户端 JAR、语言文件。`MINEINTENT_CACHE_DIR`，
//!   缺省 Linux `~/.cache/mineintent`、Windows `%LOCALAPPDATA%\mineintent`、
//!   macOS `~/Library/Caches/mineintent`。
//! - **数据**（删了就没了）：日志、每个身体的记忆与对话记录。`MINEINTENT_DATA_DIR`，
//!   缺省 Linux `~/.local/share/mineintent`、Windows `%APPDATA%\mineintent`、
//!   macOS `~/Library/Application Support/mineintent`。
//!
//! 数据目录布局：
//! ```text
//! logs/latest.log                     本次运行的控制台输出
//! logs/<开始时间>.log                 之前几次运行的
//! profiles/<用户名>/memory.md         这个身体的长期记忆（内置模型入口）
//! profiles/<用户名>/sessions/<开始时间>.log   每次运行的对话记录
//! ```
//! 多开时按用户名分开，互不覆盖。

use std::path::PathBuf;

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    var("HOME")
}

/// 自管缓存的根目录。
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = var("MINEINTENT_CACHE_DIR") {
        return Some(dir);
    }
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        home()?.join("Library").join("Caches")
    } else {
        var("XDG_CACHE_HOME").or_else(|| home().map(|home| home.join(".cache")))?
    };
    Some(base.join("mineintent"))
}

/// 自管数据的根目录。
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = var("MINEINTENT_DATA_DIR") {
        return Some(dir);
    }
    let base = if cfg!(windows) {
        var("APPDATA")?
    } else if cfg!(target_os = "macos") {
        home()?.join("Library").join("Application Support")
    } else {
        var("XDG_DATA_HOME").or_else(|| home().map(|home| home.join(".local").join("share")))?
    };
    Some(base.join("mineintent"))
}

/// 一个身体自己的数据目录。用户名只收原版允许的字符（字母、数字、下划线），
/// 其余替换成下划线，免得拼出别处的路径。
pub fn profile_dir(username: &str) -> Option<PathBuf> {
    Some(data_dir()?.join("profiles").join(safe_name(username)))
}

fn safe_name(username: &str) -> String {
    let name: String = username
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        "_".to_owned()
    } else {
        name
    }
}

/// 文件名用的本地时间：`2026-10-03_21-08-20`。
pub fn timestamp_name(at: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(at)
        .format("%Y-%m-%d_%H-%M-%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_cannot_escape_the_profiles_dir() {
        assert_eq!(safe_name("km_body"), "km_body");
        assert_eq!(safe_name("../etc"), "___etc");
        assert_eq!(safe_name(""), "_");
    }
}

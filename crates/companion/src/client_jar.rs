//! 画面要用的原版客户端 JAR：找到它，没有就像启动器那样从官方下载。
//!
//! 优先 `MINEINTENT_CLIENT_JAR`（设为空串表示不要画面）。没设时用自管的缓存
//! `<缓存目录>/mineintent/versions/26.1.2/client.jar`；缓存里没有，就照 PCL、HMCL
//! 等启动器的做法：读 Mojang 官方版本清单，取 26.1.2 客户端的下载地址与 SHA1，
//! 下载、校验后放进缓存。JAR 由用户本机从官方服务器取得，不随仓库分发。
//!
//! 下载失败不挡开玩：没有 JAR 就没有 `view`，原因照实打印。
//!
//! 外接入口（MCP）总是准备 JAR：客户端和它选的模型收不收图片，由它们自己定。
//! 内置入口走 Chat 协议时不自动找缓存、不下载，只认显式设的路径：Chat Completions
//! 的工具回执只能是文本，而内置适配器把画面放在工具回执里，这条路上用不上。

use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

/// 画面资源必须与协议同一版本（`vision` 的转录按它来）。
pub const CLIENT_VERSION: &str = "26.1.2";

const VERSION_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// 找客户端 JAR。返回 None 表示这次没有画面，原因已经打印。
/// `automatic`：这次配置用得上画面，没设路径时可以用缓存、必要时下载。
pub async fn locate(automatic: bool) -> Option<PathBuf> {
    match std::env::var_os("MINEINTENT_CLIENT_JAR") {
        Some(path) if path.is_empty() => {
            println!("[组合根] MINEINTENT_CLIENT_JAR 为空：不提供画面工具");
            return None;
        }
        Some(path) => return Some(PathBuf::from(path)),
        None if !automatic => return None,
        None => {}
    }
    let Some(target) = cached_path() else {
        println!(
            "[组合根] 找不到缓存目录，不提供画面工具；可设 MINEINTENT_CLIENT_JAR 指向本地 \
{CLIENT_VERSION} 客户端 JAR"
        );
        return None;
    };
    if target.is_file() {
        return Some(target);
    }
    println!(
        "[组合根] 首次使用：从 Mojang 官方下载 {CLIENT_VERSION} 客户端 JAR 到 {}",
        target.display()
    );
    match download(&target).await {
        Ok(()) => {
            println!("[组合根] 客户端 JAR 已下载并通过 SHA1 校验");
            Some(target)
        }
        Err(reason) => {
            println!(
                "[组合根] 下载客户端 JAR 失败：{reason}。这次不提供画面工具；可稍后重启再试，\
或设 MINEINTENT_CLIENT_JAR 指向本地 {CLIENT_VERSION} 客户端 JAR"
            );
            None
        }
    }
}

/// 自管缓存里这个版本的目录（缓存根见 [`crate::paths::cache_dir`]）。
pub(crate) fn version_cache_dir() -> Option<PathBuf> {
    Some(
        crate::paths::cache_dir()?
            .join("versions")
            .join(CLIENT_VERSION),
    )
}

/// 自管缓存里的 JAR 位置。
fn cached_path() -> Option<PathBuf> {
    Some(version_cache_dir()?.join("client.jar"))
}

/// 官方清单里这个版本客户端的下载信息。
#[derive(Debug, PartialEq)]
struct ClientDownload {
    url: String,
    sha1: String,
    size: u64,
}

async fn download(target: &Path) -> Result<(), String> {
    let http = reqwest::Client::new();
    let version = version_json(&http).await?;
    let client = client_download(&version)?;
    let bytes = fetch_verified(&http, &client.url, &client.sha1, Some(client.size)).await?;
    write_atomically(target, &bytes)
}

/// 这个版本的官方版本详情（含客户端下载与资源索引地址）。
pub(crate) async fn version_json(http: &reqwest::Client) -> Result<serde_json::Value, String> {
    let manifest = get_json(http, VERSION_MANIFEST).await?;
    let version_url = version_url(&manifest, CLIENT_VERSION)?;
    get_json(http, &version_url).await
}

/// 下载并核对 SHA1（给了大小也核对大小）。
pub(crate) async fn fetch_verified(
    http: &reqwest::Client,
    url: &str,
    sha1: &str,
    size: Option<u64>,
) -> Result<Vec<u8>, String> {
    let bytes = http
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("下载 {url} 失败：{error}"))?
        .bytes()
        .await
        .map_err(|error| format!("读取下载内容失败：{error}"))?;
    if let Some(size) = size.filter(|size| bytes.len() as u64 != *size) {
        return Err(format!(
            "大小不对：应为 {size} 字节，收到 {} 字节",
            bytes.len()
        ));
    }
    let digest = sha1_hex(&bytes);
    if !digest.eq_ignore_ascii_case(sha1) {
        return Err(format!("SHA1 不对：应为 {sha1}，算得 {digest}"));
    }
    Ok(bytes.to_vec())
}

/// 写进缓存：先写临时文件再改名，半截文件永远不会以正式名字出现。
pub(crate) fn write_atomically(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = target.parent().ok_or("缓存路径没有上级目录")?;
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("建目录 {} 失败：{error}", dir.display()))?;
    // 临时名带进程号：多开时两个进程同时首次下载，不会写进同一个半截文件。
    let mut partial = target.as_os_str().to_owned();
    partial.push(format!(".{}.part", std::process::id()));
    let partial = PathBuf::from(partial);
    std::fs::write(&partial, bytes)
        .map_err(|error| format!("写入 {} 失败：{error}", partial.display()))?;
    std::fs::rename(&partial, target)
        .map_err(|error| format!("改名为 {} 失败：{error}", target.display()))
}

pub(crate) async fn get_json(
    http: &reqwest::Client,
    url: &str,
) -> Result<serde_json::Value, String> {
    http.get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("请求 {url} 失败：{error}"))?
        .json()
        .await
        .map_err(|error| format!("{url} 不是可读的 JSON：{error}"))
}

/// 版本清单里某个版本的详情地址。
fn version_url(manifest: &serde_json::Value, id: &str) -> Result<String, String> {
    manifest["versions"]
        .as_array()
        .and_then(|versions| versions.iter().find(|version| version["id"] == id))
        .and_then(|version| version["url"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("官方版本清单里没有 {id}"))
}

/// 版本详情里客户端 JAR 的下载地址、SHA1 与大小。
fn client_download(version: &serde_json::Value) -> Result<ClientDownload, String> {
    let client = &version["downloads"]["client"];
    match (
        client["url"].as_str(),
        client["sha1"].as_str(),
        client["size"].as_u64(),
    ) {
        (Some(url), Some(sha1), Some(size)) => Ok(ClientDownload {
            url: url.to_owned(),
            sha1: sha1.to_ascii_lowercase(),
            size,
        }),
        _ => Err("版本详情里没有客户端 JAR 的下载信息".to_owned()),
    }
}

fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn finds_the_version_in_the_manifest() {
        let manifest = json!({"versions": [
            {"id": "26.1.3", "url": "https://example/26.1.3.json"},
            {"id": "26.1.2", "url": "https://example/26.1.2.json"}
        ]});
        assert_eq!(
            version_url(&manifest, "26.1.2").unwrap(),
            "https://example/26.1.2.json"
        );
        assert!(version_url(&manifest, "1.21.1").is_err());
    }

    #[test]
    fn reads_the_client_download() {
        let version = json!({"downloads": {"client": {
            "url": "https://example/client.jar", "sha1": "ABCDEF", "size": 3
        }}});
        assert_eq!(
            client_download(&version).unwrap(),
            ClientDownload {
                url: "https://example/client.jar".to_owned(),
                sha1: "abcdef".to_owned(),
                size: 3,
            }
        );
        assert!(client_download(&json!({"downloads": {}})).is_err());
    }

    #[test]
    fn sha1_is_lowercase_hex() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}

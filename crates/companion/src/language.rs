//! 语言文件：让系统消息和物品、方块、实体名按玩家选的语言出字。
//!
//! `MINEINTENT_LANG` 选语言，缺省 `zh_cn`；设为空串表示不翻译（只出注册名与
//! Azalea 自带的英文）。`en_us` 在客户端 JAR 里；其余语言和启动器一样从官方
//! 资源索引取：版本详情 → 资源索引 → `resources.download.minecraft.net` 上按
//! SHA1 存放的对象，校验后缓存到 `<缓存目录>/versions/26.1.2/lang/<code>.json`。
//!
//! 取不到不挡开玩：退回 JAR 里的 `en_us`，再不行就不翻译，原因照实打印。

use std::path::PathBuf;

use world::lang::Language;

use crate::client_jar;

const OBJECTS: &str = "https://resources.download.minecraft.net";

/// 按配置装进程级语言表。必须在连接服务器前调用。
pub async fn install(jar: Option<&mut vision::Resources>) {
    let code = std::env::var("MINEINTENT_LANG")
        .unwrap_or_else(|_| "zh_cn".to_owned())
        .trim()
        .to_ascii_lowercase();
    if code.is_empty() {
        println!("[组合根] MINEINTENT_LANG 为空：不翻译，名字只出注册名");
        return;
    }
    let loaded = if code == "en_us" {
        from_jar(jar, &code)
    } else {
        match from_cache_or_download(&code).await {
            Ok(language) => Ok(language),
            Err(reason) => {
                println!("[组合根] 取语言文件 {code} 失败：{reason}。退回 en_us");
                from_jar(jar, "en_us")
            }
        }
    };
    match loaded {
        Ok(language) => {
            world::lang::install(language);
            println!("[组合根] 语言：{code}");
        }
        Err(reason) => println!("[组合根] 没有可用的语言文件（{reason}），不翻译"),
    }
}

fn from_jar(jar: Option<&mut vision::Resources>, code: &str) -> Result<Language, String> {
    let jar = jar.ok_or("没有客户端 JAR")?;
    Language::from_json(&jar.language_file(code)?)
}

fn cached_path(code: &str) -> Option<PathBuf> {
    Some(
        client_jar::version_cache_dir()?
            .join("lang")
            .join(format!("{code}.json")),
    )
}

async fn from_cache_or_download(code: &str) -> Result<Language, String> {
    // 语言代码进文件名和索引键：只收原版那样的小写字母、数字和下划线。
    if !code
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!("语言代码 {code} 不像原版的（如 zh_cn、ja_jp）"));
    }
    let target = cached_path(code).ok_or("找不到缓存目录")?;
    if let Ok(bytes) = std::fs::read(&target) {
        if let Ok(language) = Language::from_json(&bytes) {
            return Ok(language);
        }
    }
    println!(
        "[组合根] 从 Mojang 官方资源下载语言文件 {code} 到 {}",
        target.display()
    );
    let http = reqwest::Client::new();
    let version = client_jar::version_json(&http).await?;
    let index_url = version["assetIndex"]["url"]
        .as_str()
        .ok_or("版本详情里没有资源索引地址")?;
    let index = client_jar::get_json(&http, index_url).await?;
    let object = language_object(&index, code)?;
    let url = format!("{OBJECTS}/{}/{}", &object.hash[..2], object.hash);
    let bytes = client_jar::fetch_verified(&http, &url, &object.hash, Some(object.size)).await?;
    let language = Language::from_json(&bytes)?;
    client_jar::write_atomically(&target, &bytes)?;
    Ok(language)
}

#[derive(Debug, PartialEq)]
struct Object {
    hash: String,
    size: u64,
}

/// 资源索引里某个语言文件的对象哈希与大小。
fn language_object(index: &serde_json::Value, code: &str) -> Result<Object, String> {
    let entry = &index["objects"][format!("minecraft/lang/{code}.json")];
    match (entry["hash"].as_str(), entry["size"].as_u64()) {
        (Some(hash), Some(size))
            if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            Ok(Object {
                hash: hash.to_ascii_lowercase(),
                size,
            })
        }
        _ => Err(format!("官方资源索引里没有语言 {code}")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn finds_the_language_in_the_index() {
        let index = json!({"objects": {"minecraft/lang/zh_cn.json": {
            "hash": "F87510F4509890EAF176E0DE1430F6BB326A6800", "size": 555146
        }}});
        assert_eq!(
            language_object(&index, "zh_cn").unwrap(),
            Object {
                hash: "f87510f4509890eaf176e0de1430f6bb326a6800".to_owned(),
                size: 555146,
            }
        );
        assert!(language_object(&index, "xx_yy").is_err());
    }
}

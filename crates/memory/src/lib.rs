//! 记忆：单文件长期记忆。
//!
//! 一个文件、两张脸、一个出口：工具脸（`remember` 整文改写，Free 类）和
//! 策略脸（上下文模块压缩时落盘）都经由 [`MemoryFile::write`] 写入；
//! 读取只有上下文组装一处，每轮现读，外部改动自下次读取生效。
//!
//! 记忆免压缩不靠豁免规则：全文活在基础上下文里，而压缩只碰会话区。

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{ToolClass, ToolProvider};
use serde_json::{json, Value};

/// 记忆文件的文件语义：整文读、整文原子写。
pub struct MemoryFile {
    path: PathBuf,
    /// 串行化写入方（工具脸与策略脸），避免并发写共用同一个临时文件。
    write_guard: Mutex<()>,
}

impl MemoryFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write_guard: Mutex::new(()),
        }
    }

    /// 整文现读。文件尚不存在视为空记忆，不算错误。
    pub fn read(&self) -> io::Result<String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error),
        }
    }

    /// 整文原子替换：写同目录临时文件再改名，读方永远看不到半截内容。
    pub fn write(&self, full_text: &str) -> io::Result<()> {
        let _serialized = self.write_guard.lock().expect("记忆写锁中毒");
        let mut staging = self.path.clone().into_os_string();
        staging.push(".staging");
        let staging = PathBuf::from(staging);
        std::fs::write(&staging, full_text)?;
        std::fs::rename(&staging, &self.path)
    }
}

const TOOL_NAME: &str = "remember";

/// 注册进编排的工具脸。
pub struct MemoryTools {
    file: Arc<MemoryFile>,
}

impl MemoryTools {
    pub fn new(file: Arc<MemoryFile>) -> Self {
        Self { file }
    }

    fn remember(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(full_text) = call
            .arguments
            .as_object()
            .and_then(|arguments| arguments.get("full_text"))
            .and_then(Value::as_str)
        else {
            return ToolResult::failure(call_id, "remember 需要字符串参数 full_text；请改写调用");
        };
        match self.file.write(full_text) {
            Ok(()) => ToolResult::success_json(call_id, json!({ "state": "saved" })),
            Err(error) => ToolResult::failure(call_id, format!("记忆写入失败：{error}")),
        }
    }
}

/// 注册进编排的身份：Free 类——不占域、不受界面压制。
impl ToolProvider for MemoryTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "full_text": {
                        "type": "string",
                        "description": "记忆的完整新全文。要追加就把当前全文带上一起写回，要删改就写回改后的全文"
                    }
                },
                "required": ["full_text"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "改写长期记忆。当前记忆全文每轮都在你的上下文里；此工具用 full_text 整体替换它，\
写入立即落盘，跨压缩与重启存续。"
                .to_owned(),
        );
        vec![(definition, ToolClass::Free)]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move { self.remember(call) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use agent::{ContentPart, ToolResultStatus};

    use super::*;

    /// 每个测试独占一个目录；进程号+计数器保证并行测试互不相扰。
    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mineintent-memory-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("建测试目录");
        dir
    }

    fn json_payload(result: &ToolResult) -> &Value {
        match &result.content[0] {
            ContentPart::Json { value } => value,
            other => panic!("期望 JSON 结果，得到 {other:?}"),
        }
    }

    #[test]
    fn missing_file_reads_as_empty_memory() {
        let file = MemoryFile::new(scratch_dir().join("memory.md"));
        assert_eq!(file.read().unwrap(), "");
    }

    #[test]
    fn write_then_read_roundtrips_and_leaves_no_staging_file() {
        let dir = scratch_dir();
        let file = MemoryFile::new(dir.join("memory.md"));
        file.write("我叫小明，住在山坡的木屋里。\n").unwrap();

        assert_eq!(file.read().unwrap(), "我叫小明，住在山坡的木屋里。\n");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("memory.md")]);
    }

    #[test]
    fn external_edits_show_up_on_next_read() {
        let dir = scratch_dir();
        let path = dir.join("memory.md");
        let file = MemoryFile::new(path.clone());
        file.write("旧记忆").unwrap();

        std::fs::write(&path, "维护者手改的记忆").unwrap();
        assert_eq!(file.read().unwrap(), "维护者手改的记忆");
    }

    #[tokio::test]
    async fn remember_replaces_the_whole_text() {
        let file = Arc::new(MemoryFile::new(scratch_dir().join("memory.md")));
        file.write("要被整个换掉的旧文").unwrap();
        let tools = MemoryTools::new(file.clone());

        let result = tools
            .call(ToolCall::new(
                "call-1",
                TOOL_NAME,
                json!({ "full_text": "新的全文" }),
            ))
            .await;

        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(json_payload(&result)["state"], "saved");
        assert_eq!(file.read().unwrap(), "新的全文");
    }

    #[tokio::test]
    async fn remember_without_full_text_comes_back_as_rewrite_hint() {
        let tools = MemoryTools::new(Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))));

        let missing = tools
            .call(ToolCall::new("call-1", TOOL_NAME, json!({})))
            .await;
        assert_eq!(missing.status, ToolResultStatus::Error);

        let wrong_type = tools
            .call(ToolCall::new(
                "call-2",
                TOOL_NAME,
                json!({ "full_text": 42 }),
            ))
            .await;
        assert_eq!(wrong_type.status, ToolResultStatus::Error);
    }

    #[test]
    fn registers_one_free_ascii_named_tool() {
        let tools = MemoryTools::new(Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))));
        let registered = tools.tools();

        assert_eq!(registered.len(), 1);
        let (definition, class) = &registered[0];
        assert_eq!(definition.name.as_str(), "remember");
        assert_eq!(*class, ToolClass::Free);
        assert!(definition
            .name
            .as_str()
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
    }
}

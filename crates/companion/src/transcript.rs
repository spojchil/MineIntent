//! 对话记录：每次运行一个文件，记下模型（或外接代理）说了什么、调了哪些工具、
//! 工具回了什么，每行带本地时间。
//!
//! 缺省写到 `<数据目录>/profiles/<用户名>/sessions/<开始时间>.log`
//! （见 [`crate::paths`]）；`MINEINTENT_TRACE_FILE` 可改到别处，设为空串表示不记。
//! 内容**未经脱敏**（聊天原文都在里面），只在本机；外传前自己看一眼。
//!
//! 不记 `ModelRequestTranscript`——那是每次请求的整份上下文，量级完全不同，
//! 要看那个另说。这里只回答「它做了什么」。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 打开这次运行的对话记录。返回 None 表示不记（原因已打印）。
pub fn open(username: &str) -> Option<std::sync::Arc<Transcript>> {
    let path = match std::env::var_os("MINEINTENT_TRACE_FILE") {
        Some(path) if path.is_empty() => {
            println!("[组合根] MINEINTENT_TRACE_FILE 为空：不记对话");
            return None;
        }
        Some(path) => PathBuf::from(path),
        None => {
            let Some(dir) = crate::paths::profile_dir(username) else {
                println!("[组合根] 找不到数据目录，不记对话；可设 MINEINTENT_DATA_DIR");
                return None;
            };
            let name = crate::paths::timestamp_name(std::time::SystemTime::now());
            dir.join("sessions").join(format!("{name}.log"))
        }
    };
    match Transcript::open(&path) {
        Ok(transcript) => {
            println!("[组合根] 对话记录：{}（内容未脱敏）", path.display());
            Some(std::sync::Arc::new(transcript))
        }
        Err(reason) => {
            println!("[组合根] {reason}；这次不记对话");
            None
        }
    }
}

pub struct Transcript(std::sync::Mutex<std::fs::File>);

impl Transcript {
    fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("建对话记录目录 {} 失败：{error}", dir.display()))?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(|file| Self(std::sync::Mutex::new(file)))
            .map_err(|error| format!("对话记录文件打不开（{}）：{error}", path.display()))
    }

    pub fn write(&self, line: &str) {
        let at = chrono::Local::now().format("%H:%M:%S");
        // 记录失败不该拖垮同伴：写不进去就算了，别 panic 进观察端旁路。
        if let Ok(mut file) = self.0.lock() {
            let _ = writeln!(file, "{at} {line}");
            let _ = file.flush();
        }
    }

    /// 一次工具调用。
    pub fn call(&self, call: &agent::ToolCall) {
        self.write(&format!("[调用] {} {}", call.name.as_str(), call.arguments));
    }

    /// 一条工具回执。图片只记「[图片]」，不展开。
    pub fn result(&self, result: &agent::ToolResult) {
        let body: String = result
            .content
            .iter()
            .map(|part| match part {
                agent::ContentPart::Text { text } => text.clone(),
                agent::ContentPart::Json { value } => value.to_string(),
                agent::ContentPart::Image { .. } => "[图片]".to_owned(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.write(&format!("[回执/{:?}] {body}", result.status));
    }
}

/// 每次模型请求一行：这一份上下文有多大、命中了多少、花了多久。
/// 每一轮的起止各一行：它怎么开始的、怎么结束的。
///
/// 轮级的 `ModelUsage` 是**累加**的（midturn `types.rs` 的 merge），回答不了
/// 「单次请求的上下文多大」——那正是评估上下文时唯一要看的数。逐请求的
/// `ModelRequestFinished` 才带真实数字。
///
/// 轮的生命周期事件（`RunStarted` / `RunCompleted` / `RunStopped` / `RunFailed`）
/// 必须接住：没有它们，一轮卡住时轨迹上只剩请求行，看不出这轮再也没结束。
///
/// 观察端是旁路，补记不改任何行为。
impl agent::Observer for Transcript {
    fn observe(&self, event: &agent::AgentEvent) {
        match event {
            agent::AgentEvent::ModelRequestStarted {
                request_index,
                transcript_items,
                function_tools,
                ..
            } => self.write(&format!(
                "[请求#{request_index}] 转录条目={transcript_items} 工具={function_tools}"
            )),
            agent::AgentEvent::ModelRequestFinished {
                request_index,
                duration_ms,
                usage,
                ..
            } => {
                let (input, cached, output) = usage
                    .as_ref()
                    .map(|u| {
                        (
                            u.input_tokens.unwrap_or(0),
                            u.cached_input_tokens.unwrap_or(0),
                            u.output_tokens.unwrap_or(0),
                        )
                    })
                    .unwrap_or((0, 0, 0));
                self.write(&format!(
                    "[用量#{request_index}] 输入={input} 命中={cached} 未命中={} 输出={output} 耗时={duration_ms}ms",
                    input.saturating_sub(cached)
                ));
            }
            agent::AgentEvent::RunStarted {
                run_id,
                prior_transcript_items,
                ..
            } => self.write(&format!(
                "[轮开始] id={run_id:?} 起始转录条目={prior_transcript_items}"
            )),
            agent::AgentEvent::RunCompleted {
                run_id,
                model_requests,
                tool_batches,
                ..
            } => self.write(&format!(
                "[轮结束] id={run_id:?} 结局=完成 请求数={model_requests} 工具批={tool_batches}"
            )),
            agent::AgentEvent::RunStopped { run_id, .. } => {
                self.write(&format!("[轮结束] id={run_id:?} 结局=被停止"))
            }
            agent::AgentEvent::RunFailed {
                run_id,
                stage,
                error_kind,
                ..
            } => self.write(&format!(
                "[轮结束] id={run_id:?} 结局=失败 阶段={stage:?} 错误类别={error_kind:?}"
            )),
            _ => {}
        }
    }
}

impl agent::ContentObserver for Transcript {
    fn observe(&self, event: &agent::ContentEvent) {
        match event {
            agent::ContentEvent::ModelResponseOutput { output, .. } => {
                for part in &output.content {
                    if let agent::ContentPart::Text { text } = part {
                        self.write(&format!("[说] {text}"));
                    }
                }
                for call in &output.tool_calls {
                    self.call(call);
                }
            }
            agent::ContentEvent::ToolBatchResults { results, .. } => {
                for result in &results.results {
                    self.result(result);
                }
            }
            _ => {}
        }
    }
}

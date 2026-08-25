//! 让时间过去：一件什么都不做的工具。
//!
//! # 为什么需要它
//!
//! 在此之前，模型手里**没有「等」这个动作**。想让时间过去只能调工具——第四跑
//! 里它调了 95 次 `jobs {"action":"list"}`，而 `jobs` 的说明第一句就写着
//! 「不要轮询」。那不是它不听话，是我们没给它别的办法：不调工具，这一轮就
//! 结束了，而下一轮要等到有人叫醒它。
//!
//! # 为什么必须可打断
//!
//! 一个阻塞的等待会变成新的丢通知窗口：等 60 秒的中途有人说话、有怪咬它、
//! 任务出了终局，它得等剩下的 55 秒才知道。所以 `wait` **永远可打断**，
//! 而且这不是它能选的——见下。
//!
//! ## 为什么不给「是否允许通知」参数
//!
//! 「什么可以打断我」是未裁的关注清单（issue #135）。在本工具的签名里塞一个
//! 布尔，等于在代码里静默决定它的一部分（G05）。等 #135 落地，可打断的集合
//! 应当定义在那里，而不是长在这件工具的参数上。
//!
//! 默认永远可打断在产品上也站得住：对一个以共同经历为存在理由的主体，
//! **被打断就是内容本身**（P01）。
//!
//! # 打断的判据：不是「等待期间有事」，是「有它还没看见的事」
//!
//! 天真的写法是等待期间挂一个通知回调。它会漏掉一段：模型**想**等的那一刻，
//! 到 `wait` 真正开始等，中间隔着一次模型推理（实测 3–4 秒）。这段时间里
//! 送达的唤醒已经并进了这一轮，模型却要等到 `wait` 返回才看得见。
//!
//! 所以判据取两个计数的差：**每投递一批唤醒**记一次数，每开始一次模型请求把当时
//! 的计数拍一张快照。**计数比快照大 = 有它没看过的信**，那就一秒都不该等。
//!
//! 记数的地方不在本 crate，也不在内核。这里只把问题定义成 [`Interruptions`]，
//! 由组合根回答——**不能听内核的 `MailboxEnqueued`**：帧和唤醒走的是同一条投递
//! 通道，那个事件分不出是哪一种，而帧不该打断等待（帧的闸门是「身体还在动」，
//! 拿它当打断就退回轮询）。完整理由在组合根的 `doorbell` 模块头。
//!
//! 这与「推事件、拉状态」是同一条纪律的另一面：模型的时间锚是它自己的动作，
//! 任何「现在」的判断都必须钉在某个动作上，不能钉在墙钟上。

use std::sync::Arc;
use std::time::Duration;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{ToolClass, ToolProvider};
use serde_json::{json, Value};

const TOOL_NAME: &str = "wait";

/// 上限。再长就不是「等一会儿」而是「离线」，那是另一件事（见 `presence`）。
const MAX_SECONDS: u64 = 300;

/// 一次等待的结局。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Woke {
    /// 等满了，其间没有需要叫醒它的事。
    ///
    /// **不等于「什么都没发生」**：身体照常动，在途任务的进展照常成帧投递，只是
    /// 那些不叫醒人。措辞必须留住这个区别，否则回执会和紧随其后的进展行打架。
    Timeout,
    /// 有它还没看见的信件，提前醒。
    Interrupted,
}

/// 打断源：唯一要外面回答的问题是「有没有它还没看见的信」。
///
/// 故意不叫「有没有新消息」——两者差着模型推理那一段（见模块头）。
pub trait Interruptions: Send + Sync {
    /// 等到出现模型还没看见的信件，或 `at_most` 到期。
    ///
    /// 实现必须在**开始等之前**先答一次：已经有没看过的信就立刻返回
    /// [`Woke::Interrupted`]，一秒都不等。
    fn until_woken<'a>(&'a self, at_most: Duration) -> PortFuture<'a, Woke>;
}

pub struct WaitTools {
    interruptions: Arc<dyn Interruptions>,
}

impl WaitTools {
    pub fn new(interruptions: Arc<dyn Interruptions>) -> Self {
        Self { interruptions }
    }

    async fn dispatch_wait(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        let Some(seconds) = arguments.get("seconds").and_then(Value::as_u64) else {
            return ToolResult::failure(call_id, "wait 需要 seconds：等几秒（整数）");
        };
        if seconds == 0 || seconds > MAX_SECONDS {
            return ToolResult::failure(
                call_id,
                format!("seconds 要在 1..={MAX_SECONDS} 之间；想更久就多等几次"),
            );
        }

        let at = std::time::Instant::now();
        let woke = self
            .interruptions
            .until_woken(Duration::from_secs(seconds))
            .await;
        // 报**实际**等了多久，不报请求的时长：被打断时两者不一样，
        // 而模型据以推断「刚才那件事是什么时候发生的」。
        let waited = at.elapsed().as_secs();
        ToolResult::success(
            call_id,
            vec![agent::ContentPart::text(match woke {
                // 不复述唤醒的内容：那些信件本来就并进了这一轮，它自己看得见。
                // 这里只说明「为什么醒」，说两遍反而像是两件事。
                Woke::Interrupted => {
                    format!("等了 {waited} 秒，有事发生，提前醒了——就在下面。")
                }
                Woke::Timeout => {
                    format!("等满 {seconds} 秒，没有需要叫醒你的事。")
                }
            })],
        )
    }
}

/// 注册身份：`Free`。不占身体的任何一域；死着、开着界面，都该能让时间过去。
impl ToolProvider for WaitTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "seconds": {
                        "type": "integer",
                        "description": format!("等几秒，1..={MAX_SECONDS}"),
                    }
                },
                "required": ["seconds"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "什么都不做，让时间过去。\
**有事发生会立刻把你叫醒**——别人说话、你受伤、你下的任务有了结果，都会打断等待，\
不用担心睡过去。所以在等某件事的时候用它，别反复查同一件工具。\
等待期间身体照常：在走的路继续走，在挖的方块继续挖。"
                .to_owned(),
        );
        vec![(definition, ToolClass::Free)]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move { self.dispatch_wait(call).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use agent::{ContentPart, ToolResultStatus};

    use super::*;

    struct FakeBell {
        answer: Woke,
        /// 每次被问到时记下要求的时长，用来验「立刻返回」不是靠睡满。
        asked: Mutex<Vec<Duration>>,
        calls: AtomicU64,
    }

    impl FakeBell {
        fn new(answer: Woke) -> Arc<Self> {
            Arc::new(Self {
                answer,
                asked: Mutex::new(Vec::new()),
                calls: AtomicU64::new(0),
            })
        }
    }

    impl Interruptions for FakeBell {
        fn until_woken<'a>(&'a self, at_most: Duration) -> PortFuture<'a, Woke> {
            self.asked.lock().unwrap().push(at_most);
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move { self.answer })
        }
    }

    fn call(seconds: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, json!({ "seconds": seconds }))
    }

    fn text_of(result: &ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// 等满了只能说「没有需要叫醒你的事」，不能说「什么都没发生」。
    ///
    /// 帧不敲铃（那是对的），所以等待期间在途任务的进展照常成帧，并在 `wait`
    /// 返回后紧接着投递。说「什么都没发生」，模型下一行就会读到方块又碎了两块。
    #[tokio::test]
    async fn waiting_out_the_full_span_claims_only_that_nothing_woke_it() {
        let bell = FakeBell::new(Woke::Timeout);
        let tools = WaitTools::new(bell.clone());
        let result = tools.dispatch_wait(call(json!(30))).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        let text = text_of(&result);
        assert!(text.contains("等满 30 秒"), "{text}");
        assert!(text.contains("没有需要叫醒你的事"), "{text}");
        assert!(
            !text.contains("什么都没发生"),
            "身体照常动、进展照常成帧，不能说什么都没发生：{text}"
        );
        assert_eq!(
            bell.asked.lock().unwrap().as_slice(),
            &[Duration::from_secs(30)]
        );
    }

    /// 被打断时报的是**实际等了多久**，不是请求的时长——模型据此推断
    /// 「刚才那件事大概什么时候发生的」。
    #[tokio::test]
    async fn an_interrupted_wait_reports_why_and_points_at_what_follows() {
        let tools = WaitTools::new(FakeBell::new(Woke::Interrupted));
        let text = text_of(&tools.dispatch_wait(call(json!(300))).await);
        assert!(text.contains("提前醒"), "{text}");
        // 不复述唤醒内容：那些信件本来就并进同一轮，说两遍像两件事。
        assert!(!text.contains("等满"), "{text}");
        assert!(text.contains("就在下面"), "{text}");
    }

    /// 上限与下限都要拒得能据以改写，不能只说「参数错误」。
    #[tokio::test]
    async fn out_of_range_spans_are_refused_with_the_bounds_named() {
        let tools = WaitTools::new(FakeBell::new(Woke::Timeout));
        for bad in [json!(0), json!(301), json!(100000)] {
            let result = tools.dispatch_wait(call(bad.clone())).await;
            assert_eq!(result.status, ToolResultStatus::Error, "{bad} 应当被拒");
            let text = text_of(&result);
            assert!(text.contains("300"), "拒绝话术要点出上限：{text}");
        }
    }

    #[tokio::test]
    async fn a_missing_span_is_refused_instead_of_defaulted() {
        let tools = WaitTools::new(FakeBell::new(Woke::Timeout));
        let result = tools
            .dispatch_wait(ToolCall::new("call-1", TOOL_NAME, json!({})))
            .await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(text_of(&result).contains("seconds"), "要点名缺哪个参数");
    }

    /// 注册身份是 `Free`：不占域，死着与开着界面都放行。
    /// 等待被生命闸门拦下会让死亡变成一段无法度过的时间。
    #[test]
    fn the_tool_is_free_so_death_and_open_screens_do_not_block_it() {
        let tools = WaitTools::new(FakeBell::new(Woke::Timeout));
        let registered = tools.tools();
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0.name.as_str(), "wait");
        assert!(matches!(registered[0].1, ToolClass::Free));
    }

    /// 工具说明必须自己说清「会被叫醒」。没有这句，模型会继续轮询——
    /// 它轮询不是因为不听话，是因为不敢睡。
    #[test]
    fn the_description_promises_interruption_and_says_not_to_poll() {
        let tools = WaitTools::new(FakeBell::new(Woke::Timeout));
        let described = tools.tools()[0].0.description.clone().unwrap();
        assert!(described.contains("叫醒"), "{described}");
        assert!(described.contains("别反复查"), "{described}");
    }
}

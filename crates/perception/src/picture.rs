//! On-demand first-person picture; the composition root supplies rendered PNG.
use std::sync::Arc;

use agent::{ContentPart, PortFuture, ToolCall, ToolDefinition, ToolResult};
use base64::{engine::general_purpose::STANDARD, Engine};
use dispatch::{ToolClass, ToolProvider};
use serde_json::json;

pub struct Picture {
    pub png: Vec<u8>,
    /// Visible-image limitations only; never an inventory of the captured region.
    pub description: String,
}

pub trait PictureDoor: Send + Sync {
    fn capture<'a>(&'a self) -> PortFuture<'a, Result<Picture, String>>;
}

pub struct PictureTools {
    door: Arc<dyn PictureDoor>,
}

impl PictureTools {
    pub fn new(door: Arc<dyn PictureDoor>) -> Self {
        Self { door }
    }
}

impl ToolProvider for PictureTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            "view",
            json!({
                "type":"object", "properties":{}, "additionalProperties":false
            }),
        );
        definition.description = Some(
            "看一张当前朝向的第一人称图片。无需参数；想换个方向看，先用 input 的 turn 转过去。准星在画面正中央，input 的鼠标键作用于它所指。\
当前只画 16 格内的方块地形，不画玩家、生物、掉落物或界面；亮度固定，不能据此判断天黑或照明。\
未绘制的内容不代表不存在。"
                .to_owned(),
        );
        vec![(definition, ToolClass::Free)]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move {
            if call.name.as_str() != "view"
                || !call
                    .arguments
                    .as_object()
                    .is_some_and(|args| args.is_empty())
            {
                return ToolResult::failure(
                    call.id,
                    "view 只接受空对象 {}，使用当前身体位置和朝向",
                );
            }
            match self.door.capture().await {
                Ok(picture) => ToolResult::success(
                    call.id,
                    vec![
                        ContentPart::text(picture.description),
                        ContentPart::image_base64("image/png", STANDARD.encode(picture.png)),
                    ],
                ),
                Err(reason) => ToolResult::failure(call.id, reason),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent::{ImageSource, ToolResultStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Door {
        calls: AtomicUsize,
        failure: bool,
    }
    impl PictureDoor for Door {
        fn capture<'a>(&'a self) -> PortFuture<'a, Result<Picture, String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if self.failure {
                    return Err("周围区块尚未加载完整，请稍后再看".to_owned());
                }
                Ok(Picture {
                    png: b"\x89PNG\r\n\x1a\n".to_vec(),
                    description: "测试图片".to_owned(),
                })
            })
        }
    }

    #[tokio::test]
    async fn picture_result_keeps_call_id_and_binary_image_separate_from_text() {
        let tools = PictureTools::new(Arc::new(Door {
            calls: AtomicUsize::new(0),
            failure: false,
        }));
        let call = ToolCall::new("picture-1", "view", json!({}));
        let id = call.id.clone();
        let result = tools.call(call).await;
        assert_eq!(result.call_id, id);
        assert_eq!(result.status, ToolResultStatus::Success);
        assert!(matches!(&result.content[0], ContentPart::Text { text } if text == "测试图片"));
        let ContentPart::Image {
            source: ImageSource::Base64 { media_type, data },
        } = &result.content[1]
        else {
            panic!("expected image part")
        };
        assert_eq!(media_type, "image/png");
        assert_eq!(STANDARD.decode(data).unwrap(), b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    async fn invalid_arguments_do_not_capture_and_failed_capture_returns_no_image() {
        let door = Arc::new(Door {
            calls: AtomicUsize::new(0),
            failure: true,
        });
        let tools = PictureTools::new(door.clone());
        for args in [json!(null), json!([]), json!({"position":[0,0,0]})] {
            assert_eq!(
                tools.call(ToolCall::new("bad", "view", args)).await.status,
                ToolResultStatus::Error
            );
        }
        assert_eq!(door.calls.load(Ordering::SeqCst), 0);
        let result = tools
            .call(ToolCall::new("unloaded", "view", json!({})))
            .await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(!result
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. })));
        assert_eq!(door.calls.load(Ordering::SeqCst), 1);
    }
}

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

/// 屏幕上的一块矩形：四个数都是占全屏的比例，原点在左上角。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub trait PictureDoor: Send + Sync {
    /// `region` 为 `None` 时出整个屏幕；否则只交屏幕上那一块（像素与全屏里那块相同）。
    fn capture<'a>(&'a self, region: Option<Region>) -> PortFuture<'a, Result<Picture, String>>;
}

/// `{}` 或 `{"region":{x,y,width,height}}`；比例越界或多余字段都拒绝。
fn parse_arguments(arguments: &serde_json::Value) -> Result<Option<Region>, String> {
    const USAGE: &str =
        "view 只接受 {} 或 {\"region\":{\"x\":…,\"y\":…,\"width\":…,\"height\":…}}；\
四个数是占全屏的比例（0–1），x、y 为左上角，矩形不能超出屏幕";
    let args = arguments.as_object().ok_or(USAGE)?;
    if args.keys().any(|key| key != "region") {
        return Err(USAGE.to_owned());
    }
    let Some(region) = args.get("region") else {
        return Ok(None);
    };
    let region = region.as_object().ok_or(USAGE)?;
    if region.len() != 4 {
        return Err(USAGE.to_owned());
    }
    let number = |key: &str| {
        region
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .ok_or(USAGE)
    };
    let region = Region {
        x: number("x")?,
        y: number("y")?,
        width: number("width")?,
        height: number("height")?,
    };
    // 比例常写成 0.66 + 0.34 这样的近似值，右下边缘留一点容差。
    const SLACK: f64 = 1e-6;
    let fits = |start: f64, size: f64| {
        (0.0..1.0).contains(&start) && size > 0.0 && start + size <= 1.0 + SLACK
    };
    if !(fits(region.x, region.width) && fits(region.y, region.height)) {
        return Err(USAGE.to_owned());
    }
    Ok(Some(region))
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
                "type":"object",
                "properties":{
                    "region":{
                        "type":"object",
                        "description":"只看屏幕上的一块；省略则看整个屏幕",
                        "properties":{
                            "x":{"type":"number","minimum":0,"maximum":1},
                            "y":{"type":"number","minimum":0,"maximum":1},
                            "width":{"type":"number","exclusiveMinimum":0,"maximum":1},
                            "height":{"type":"number","exclusiveMinimum":0,"maximum":1}
                        },
                        "required":["x","y","width","height"],
                        "additionalProperties":false
                    }
                },
                "additionalProperties":false
            }),
        );
        definition.description = Some(
            "看一张当前朝向的第一人称画面。不带参数看整个屏幕；带 region 只看屏幕上的一块：x、y 是左上角，\
width、height 是宽高，都是占全屏的比例（0–1），例如 {\"x\":0,\"y\":0,\"width\":1,\"height\":0.34} 是上面三分之一。\
局部的像素与整屏里那一块相同，不会更清楚，只是不必把整屏都传过来。\
想换个方向看，先用 input 的 turn 转过去。准星在整个屏幕的正中央，input 的鼠标键作用于它所指。\
图片旁的文字说明画了什么、没画什么；未绘制的内容不代表不存在。"
                .to_owned(),
        );
        vec![(definition, ToolClass::Free)]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move {
            if call.name.as_str() != "view" {
                return ToolResult::failure(call.id, "未知工具");
            }
            let region = match parse_arguments(&call.arguments) {
                Ok(region) => region,
                Err(usage) => return ToolResult::failure(call.id, usage),
            };
            match self.door.capture(region).await {
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
        regions: std::sync::Mutex<Vec<Option<Region>>>,
    }
    impl PictureDoor for Door {
        fn capture<'a>(
            &'a self,
            region: Option<Region>,
        ) -> PortFuture<'a, Result<Picture, String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.regions.lock().unwrap().push(region);
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
            regions: Default::default(),
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
            regions: Default::default(),
        });
        let tools = PictureTools::new(door.clone());
        for args in [
            json!(null),
            json!([]),
            json!({"position":[0,0,0]}),
            json!({"region":{"x":0,"y":0,"width":1}}),
            json!({"region":{"x":0,"y":0.5,"width":1,"height":0.6}}),
            json!({"region":{"x":-0.1,"y":0,"width":0.5,"height":0.5}}),
            json!({"region":{"x":0,"y":0,"width":0,"height":0.5}}),
            json!({"region":{"x":0,"y":0,"width":1,"height":1,"zoom":2}}),
        ] {
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

    #[tokio::test]
    async fn region_reaches_the_door_as_screen_fractions() {
        let door = Arc::new(Door {
            calls: AtomicUsize::new(0),
            failure: false,
            regions: Default::default(),
        });
        let tools = PictureTools::new(door.clone());
        let args = json!({"region":{"x":0,"y":0.66,"width":1,"height":0.34}});
        let result = tools.call(ToolCall::new("bottom", "view", args)).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        tools.call(ToolCall::new("full", "view", json!({}))).await;
        assert_eq!(
            *door.regions.lock().unwrap(),
            [
                Some(Region {
                    x: 0.0,
                    y: 0.66,
                    width: 1.0,
                    height: 0.34
                }),
                None
            ]
        );
    }
}

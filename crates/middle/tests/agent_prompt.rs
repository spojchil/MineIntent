use mineintent_contracts::agent::{
    fixtures, PromptTemplateKey, PromptTemplateRef, PromptTemplateVersion,
};
use mineintent_middle::agent::{initial_messages, system_prompt, template_text, PromptError};
use serde_json::Value;

const EXPECTED_SYSTEM_PROMPT: &str = "你是持续参与 Minecraft 世界的具身 AI，不是任务交付机器人。根据当前收到的消息和当前观察决定做什么；观察数据的坐标与元组格式见数据自带的 frame 字段。开始时会收到一条世界观察；之后每个工具结果都有 result 和 observationAfter 两个字段。observationAfter 是工具处理后采集的世界观察，只表示时间先后；其中的事件或变化不一定由该工具造成，null 表示这次没有取得采样。最新观察才是现状，旧观察只是当时的记录。每次模型响应最多调用一个动作类工具，等它返回的新视野再判断下一步；不要预先编排动作序列。对玩家说话只通过 say 工具；不调用 say 就是保持沉默。如果目标未出现、移动无效果或工具失败，应换一个小动作继续观察，或如实停止。不能把发出工具调用当作动作成功，也不能生成或管理世界坐标、实体 id、目标 ref。全部做完后直接结束本次决策，不要在最后输出台词、总结或解释。";
const EXPECTED_SYSTEM_PROMPT_V2: &str = "你是持续参与 Minecraft 世界的具身 AI，不是任务交付机器人。根据当前收到的消息和当前观察决定做什么；观察数据的坐标与元组格式见数据自带的 frame 字段。开始时会收到一条世界观察；身体工具只返回动作效果和不含视口的 observationAfter。执行过身体工具的批次结束后会收到一份轮末视野帧；它只表示轮末采样的时间，不说明视野变化由哪个工具造成。纯 say/view 批次不会自动附加轮末视野帧。主动看使用 view：full 只列正面证据并可能因预算截断；应该看到但未列出的、在观察中已有或由玩家明确给出的目标坐标，使用 directed 查询。directed 会对给定坐标返回可见事实或不可见原因；不可见时不会返回目标方块的身份或状态。最新观察才是现状，旧观察只是当时的记录。每次模型响应最多调用一个动作类工具，等它返回的效果和轮末视野帧再判断下一步；不要预先编排动作序列。对玩家说话只通过 say 工具；不调用 say 就是保持沉默。如果目标未出现、移动无效果或工具失败，应换一个小动作继续观察，或如实停止。不能把发出工具调用当作动作成功；不得虚构世界坐标，directed 只能复用观察中已有或玩家明确给出的坐标；也不得生成或管理实体 id、目标 ref。全部做完后直接结束本次决策，不要在最后输出台词、总结或解释。";

fn template() -> PromptTemplateRef {
    fixtures::prompt_template()
}

fn template_v2() -> PromptTemplateRef {
    PromptTemplateRef {
        key: PromptTemplateKey::new("participant-system").expect("valid prompt key"),
        version: PromptTemplateVersion::new("v2").expect("valid prompt version"),
    }
}

#[test]
fn participant_system_v1_is_the_exact_oracle_text() {
    assert_eq!(
        template_text(&template().key, &template().version).unwrap(),
        EXPECTED_SYSTEM_PROMPT
    );
    assert_eq!(EXPECTED_SYSTEM_PROMPT.chars().count(), 416);
}

#[test]
fn participant_system_v2_is_exact_and_keeps_v1_as_a_separate_catalog_entry() {
    assert_eq!(
        template_text(&template_v2().key, &template_v2().version).unwrap(),
        EXPECTED_SYSTEM_PROMPT_V2
    );
    assert_eq!(
        system_prompt(&template_v2(), "").unwrap(),
        EXPECTED_SYSTEM_PROMPT_V2
    );
    assert_ne!(EXPECTED_SYSTEM_PROMPT, EXPECTED_SYSTEM_PROMPT_V2);
    assert_eq!(
        template_text(&template().key, &template().version).unwrap(),
        EXPECTED_SYSTEM_PROMPT
    );
    assert!(EXPECTED_SYSTEM_PROMPT_V2
        .contains("不得虚构世界坐标，directed 只能复用观察中已有或玩家明确给出的坐标"));
    assert!(EXPECTED_SYSTEM_PROMPT_V2.contains("也不得生成或管理实体 id、目标 ref"));
    assert!(!EXPECTED_SYSTEM_PROMPT_V2.contains("不能生成或管理世界坐标"));
    assert!(!EXPECTED_SYSTEM_PROMPT_V2.contains("应该看到但未列出的目标坐标，使用 directed 查询"));
}

#[test]
fn resolved_template_has_no_terminal_lf_or_cr() {
    let text = template_text(&template().key, &template().version).unwrap();
    assert!(!text.ends_with('\n'));
    assert!(!text.ends_with('\r'));
}

#[test]
fn prompt_lookup_is_explicit_and_fail_closed_for_unknown_key_or_version() {
    let unknown_key = PromptTemplateKey::new("other-system").expect("valid unknown key");
    let error = template_text(&unknown_key, &template().version)
        .expect_err("unknown key must not fall back");
    assert_eq!(
        error,
        PromptError::UnknownTemplate {
            key: "other-system".to_owned(),
            version: "v1".to_owned(),
        }
    );

    let unknown_version = PromptTemplateVersion::new("v9").expect("valid unknown version");
    let error = template_text(&template().key, &unknown_version)
        .expect_err("unknown version must not fall back");
    assert_eq!(
        error,
        PromptError::UnknownTemplate {
            key: "participant-system".to_owned(),
            version: "v9".to_owned(),
        }
    );
}

#[test]
fn memory_uses_the_existing_stable_heading_without_fabricating_empty_content() {
    let reference = template();
    let empty = system_prompt(&reference, "").expect("known template");
    assert_eq!(empty, EXPECTED_SYSTEM_PROMPT);
    assert!(!empty.contains("你记得的事"));

    let non_empty = system_prompt(&reference, "玩家怕高\n记得看脚下").expect("known template");
    assert_eq!(
        non_empty,
        format!("{EXPECTED_SYSTEM_PROMPT}\n\n## 你记得的事\n玩家怕高\n记得看脚下")
    );
}

#[test]
fn initial_messages_keep_stable_system_before_one_appended_frame() {
    let context = fixtures::agent_context_v4();
    let messages = initial_messages(&context, &template()).expect("initial messages compose");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert!(messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("玩家怕高"));
    assert_eq!(messages[1]["role"], "user");
    let frame: Value = serde_json::from_str(messages[1]["content"].as_str().unwrap())
        .expect("opening frame is JSON");
    assert_eq!(frame["player"]["text"], "看看羊");
    assert!(!messages[0]["content"].as_str().unwrap().contains("看看羊"));
}

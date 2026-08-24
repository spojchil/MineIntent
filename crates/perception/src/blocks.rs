//! 方块记忆库的查询工具。**只有一条路：SQL。**
//!
//! # 为什么是查询而不是推送
//!
//! 方块信息**不进会话区**。逐格推送在数学上走不通：按实测 10.5 token/格，
//! 半个 1M 窗口只装得下约 4.8 万格，而站着转两分钟就攒几千格。观察照常喂
//! 记忆——那是给机器用的（寻路读它，这里查它）——但一格都不推给模型。
//!
//! 模型要方块就自己 `scan`（睁眼，把看见的吸进记忆）再来这里问。这和人改一个
//! 大仓库是同一套：从不加载全部，`grep` 到位置再读那几行。
//!
//! # 为什么只留 SQL，把 `find`/`around` 和别名表都删了
//!
//! 那两个动作和那张「木头→`_log`/`_wood`/`_stem`」的对照表，是**我们替模型
//! 收好的词**：它问「附近有什么树」，我们翻译成一组名字片段。收词看着体贴，
//! 代价有三层——
//!
//! 一、**它是一层封顶**。表里没有的说法问不出来，而表由我们维护，于是模型
//! 能问什么由我们决定。SQL 没有这个上限：`LIKE`、`GROUP BY`、自连接、坐标
//! 算术，它想得到就写得出，我们不必预先想到。
//!
//! 二、**它遮住了记忆真正的形状**。`find` 的回答是一串坐标，看不出这份记忆
//! 是三态的、有观察刻、按区段分片。模型不知道自己手里是什么，就用不好它。
//! 表结构直接摊开，它才能自己发明我们没设计过的问法——比如拿 `seen_empty`
//! 自连接找出「头顶两格确认为空」的落脚点。
//!
//! 三、**两条路会打架**。同一个问题两种问法、两套话术、两处上限，模型要先
//! 猜该走哪条。删掉快捷问法之后只剩一条路，反而没什么可犹豫的。
//!
//! 说法到方块名的翻译并没有消失，只是**换了地方**：它回到模型自己的常识里
//! （它本来就知道 `oak_log` 是木头），而不是我们表里的一行。
//!
//! # 边界
//!
//! 无论怎么问，答案只来自观察过的东西——这条边界是记忆本身的形状，不是
//! 入口的限制，所以删掉入口不会松动它。「这是悬崖」「那片林子」仍然由模型
//! 自己看出来：机器产出解释，本质是替模型下结论。

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use world::{BlockMemory, SnapshotSource};

pub(crate) const TOOL_NAME: &str = "blocks";

pub(crate) struct BlocksQuery {
    memory: Arc<Mutex<BlockMemory>>,
    snapshots: Arc<dyn SnapshotSource>,
}

impl BlocksQuery {
    pub(crate) fn new(memory: Arc<Mutex<BlockMemory>>, snapshots: Arc<dyn SnapshotSource>) -> Self {
        Self { memory, snapshots }
    }

    pub(crate) fn answer(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let Some(query) = arguments.get("query").and_then(Value::as_str) else {
            return Err("要给 query：一条 SELECT 语句（表结构见本工具的描述）".to_owned());
        };
        // **短锁取快照，查询期一律不持锁。**记忆按区段分片，克隆只复制一层
        // 指针；而 SQL 有 300ms 预算，若把锁攥到查询结束，眼睛（每 250ms 写）
        // 和寻路器（每 tick 上千次读）会一起被按住。
        let memory = Arc::new(
            self.memory
                .lock()
                .map_err(|_| "方块记忆锁中毒".to_owned())?
                .clone(),
        );
        // 判据是「一格都没观察过」，不是「记住的方块数为零」：只见过空气
        // 也是看过，那时该让查询照常跑出空结果，不是叫它去 scan。
        if memory.nothing_observed() {
            return Err("记忆库是空的——你还没看过任何东西，先 scan 一下".to_owned());
        }
        let snapshot = self.snapshots.latest();
        let position = &snapshot.self_state.position;
        crate::sql::run(memory, [position.x, position.y, position.z], query)
    }
}

pub(crate) fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "一条只读 SELECT。表结构、函数与例子见工具描述"
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

/// 常驻描述。表结构接在后面，理由见 [`crate::sql::SCHEMA_DOC`]。
pub(crate) fn description() -> String {
    format!("{HEAD}\n\n{}", crate::sql::SCHEMA_DOC)
}

const HEAD: &str = "\
用 SQL 查你自己的方块记忆——**只查得到你看过的东西**，没看过的地方它一无所知（先 scan）。\
答案是坐标与计数，怎么解读由你自己判断。不打断任何动作。\n\
**查得很快，不用替它省。** 坐标是有索引的：定点查、按坐标的连接都是直取，不是全表扫；\
想到什么就问，不用攒成一条大的，也不用先猜个范围再问。";

#[cfg(test)]
mod tests {
    use super::*;

    /// 表结构必须在**常驻描述**里，不能退回按需拉取的动作。
    ///
    /// 工具描述每次请求发一份、不累积，且不进转录、不被压缩；做成 `describe`
    /// 反而要多一次往返，拿到之后照样每次都发，还会在会话中途被压掉。
    #[test]
    fn the_table_layout_travels_with_the_tool_description() {
        let described = description();
        for table in ["seen_blocks", "seen_empty", "me"] {
            assert!(described.contains(table), "描述里缺 {table}：{described}");
        }
        assert!(described.contains("dist("), "{described}");
        // 「没看过」是补集，必须讲明白，否则模型会去 SELECT 一张不存在的表。
        assert!(described.contains("NOT EXISTS"), "{described}");
    }

    /// 成本要说出口。第四跑里模型一次都没用过这条查询，全程只靠 scan 硬看——
    /// 一个不知道代价的工具，模型会按最坏情况估，然后省着不用。既然虚表按坐标
    /// 走索引、点查是直取，就该明说，否则等于白建。
    #[test]
    fn the_description_tells_the_model_the_query_is_cheap() {
        let described = description();
        assert!(described.contains("查得很快"), "{described}");
        assert!(described.contains("索引"), "{described}");
        // 换行要真的是换行，不能是字面的反斜杠 n。
        assert!(
            !described.contains("\\n"),
            "描述里混进了字面转义：{described}"
        );
    }

    /// 别名表删干净：描述里不该再出现它，否则模型会去 JOIN 一张不存在的表。
    #[test]
    fn no_trace_of_the_alias_table_remains() {
        let described = description();
        assert!(!described.contains("block_aliases"), "{described}");
        assert!(!described.contains("alias"), "{described}");
    }

    /// 入口只剩一个：schema 里没有 action，也没有 find/around 的参数。
    #[test]
    fn the_only_way_in_is_a_query() {
        let schema = schema();
        let properties = schema["properties"].as_object().expect("properties");
        assert_eq!(
            properties.keys().collect::<Vec<_>>(),
            vec!["query"],
            "只应留下 query：{schema}"
        );
        assert_eq!(schema["required"], json!(["query"]));
    }
}

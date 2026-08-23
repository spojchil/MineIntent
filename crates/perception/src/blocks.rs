//! 方块记忆库的查询工具。
//!
//! # 为什么是查询而不是推送
//!
//! 方块信息**不进会话区**。逐格推送在数学上走不通：
//! 按实测 10.5 token/格，半个 1M 窗口只装得下约 4.8 万格，而站着转两分钟就攒
//! 几千格。差异照常算、照常喂记忆——那是给机器用的（寻路读它，这里查它）——
//! 但一格都不推给模型。
//!
//! 模型要方块就自己 `scan`（睁眼，把看见的吸进记忆）再来这里问。这和人改一个
//! 大仓库是同一套：从不加载全部，`grep` 到位置再读那几行。
//!
//! # 但这不是 grep
//!
//! grep 是**按模式列举，让调用者自己解释**。模型问的是「附近有什么树」，不是
//! `oak_log|spruce_log|birch_log`。所以入口按**用途**收词（输入侧翻译），出口
//! 一律**方块事实**——坐标、标签、方位距离。
//!
//! 「这是悬崖」「那片林子」由模型自己看出来：机器产出解释，本质是替模型下结论。
//!
//! # 三个入口，一条边界
//!
//! `find`/`around` 是收好词的快捷问法，`sql` 是把提问的自由整个交出去
//! （见 [`crate::sql`]）。三者读的是同一份记忆，因而共享同一条信息边界：
//! **只答得出观察过的东西**。

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use world::{BlockMemory, SnapshotSource};

pub(crate) const TOOL_NAME: &str = "blocks";

/// 用途 → 方块名（输入侧翻译）。
///
/// 写死一张表而不是从物品分类推：可控，改起来一眼看得见，而且模型说的「木头」
/// 与游戏的注册名本来就不是一回事。
const ALIASES: &[(&str, &[&str])] = &[
    ("木头", &["_log", "_wood", "_stem"]),
    ("树叶", &["_leaves"]),
    (
        "石头",
        &[
            "stone",
            "cobblestone",
            "deepslate",
            "andesite",
            "granite",
            "diorite",
        ],
    ),
    ("矿石", &["_ore"]),
    ("铁", &["iron_ore"]),
    ("煤", &["coal_ore"]),
    ("水", &["water"]),
    ("岩浆", &["lava"]),
    ("沙子", &["sand"]),
    ("土", &["dirt", "grass_block", "podzol", "mycelium"]),
    ("容器", &["chest", "barrel", "shulker_box", "hopper"]),
    ("炉子", &["furnace", "blast_furnace", "smoker"]),
    ("工作台", &["crafting_table"]),
    ("床", &["_bed"]),
    ("门", &["_door"]),
];

/// 把模型给的词展开成匹配片段。认别名，也认直接写的方块名。
fn expand(term: &str) -> Vec<String> {
    let lowered = term.trim().to_lowercase();
    for (alias, names) in ALIASES {
        if lowered == *alias || lowered == alias.to_lowercase() {
            return names.iter().map(|name| (*name).to_owned()).collect();
        }
    }
    vec![lowered]
}

fn matches(name: &str, needles: &[String]) -> bool {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    needles.iter().any(|needle| bare.contains(needle.as_str()))
}

fn squared(a: [i32; 3], b: [i32; 3]) -> i64 {
    let (dx, dy, dz) = (
        i64::from(b[0] - a[0]),
        i64::from(b[1] - a[1]),
        i64::from(b[2] - a[2]),
    );
    dx * dx + dy * dy + dz * dz
}

/// 从记忆里挑出符合的格位，按距离从近到远。
///
/// `range` 是格数上限；`limit` 是**结果条数**上限——一片雪原有上万格雪，
/// 全给出来就又回到了推送逐格那条死路。
fn collect(
    memory: &BlockMemory,
    origin: [i32; 3],
    keep: impl Fn(&str) -> bool,
    range: Option<i64>,
    limit: usize,
) -> Vec<([i32; 3], String)> {
    let mut found: Vec<(i64, [i32; 3], String)> = memory
        .iter()
        .filter(|(_, fact)| keep(&fact.name))
        .filter_map(|(at, fact)| {
            let distance = squared(origin, at);
            match range {
                Some(max) if distance > max * max => None,
                _ => Some((
                    distance,
                    at,
                    world::visible_block_label(&fact.name, &fact.properties),
                )),
            }
        })
        .collect();
    found.sort_by_key(|(distance, at, _)| (*distance, *at));
    found
        .into_iter()
        .take(limit)
        .map(|(_, at, label)| (at, label))
        .collect()
}

pub(crate) struct BlocksQuery {
    memory: Arc<Mutex<BlockMemory>>,
    snapshots: Arc<dyn SnapshotSource>,
}

impl BlocksQuery {
    pub(crate) fn new(memory: Arc<Mutex<BlockMemory>>, snapshots: Arc<dyn SnapshotSource>) -> Self {
        Self { memory, snapshots }
    }

    fn origin(&self) -> [i32; 3] {
        let snapshot = self.snapshots.latest();
        let position = &snapshot.self_state.position;
        [
            position.x.floor() as i32,
            position.y.floor() as i32,
            position.z.floor() as i32,
        ]
    }

    pub(crate) fn answer(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let memory = self
            .memory
            .lock()
            .map_err(|_| "方块记忆锁中毒".to_owned())?;
        if memory.is_empty() {
            return Err("记忆库是空的——你还没看过任何东西，先 scan 一下".to_owned());
        }
        let origin = self.origin();
        let range = arguments.get("range").and_then(Value::as_i64);
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(12)
            .clamp(1, 64) as usize;

        let action = arguments.get("action").and_then(Value::as_str);
        let body = match action {
            Some("around") => {
                let found = collect(&memory, origin, |_| true, range.or(Some(16)), limit);
                render::render_memory_matches(origin, &found)
            }
            Some("find") => {
                let Some(what) = arguments.get("what").and_then(Value::as_str) else {
                    return Err(
                        "find 要给 what：找什么（如「木头」「铁」，或直接写方块名）".to_owned()
                    );
                };
                let needles = expand(what);
                let found = collect(
                    &memory,
                    origin,
                    |name| matches(name, &needles),
                    range,
                    limit,
                );
                if found.is_empty() {
                    // **「记忆里没有」不等于「附近没有」。** 记忆只装看见过、
                    // 且当时露出面的方块（视口判据 ExposedFace），埋在石头里的
                    // 矿脉从来不进来。不说穿这一点，模型会把一次失败的回忆
                    // 当成一次完整的勘探，然后合理地走开——而脚下三格可能就是矿。
                    return Ok(format!(
                        "记忆里没有「{what}」。记住的一共 {} 格——这些只是你看见过、\
而且当时露出面的方块；埋在石头里的东西不会在里面，那种得挖开才知道。",
                        memory.len()
                    ));
                }
                render::render_memory_matches(origin, &found)
            }
            Some("sql") => {
                let Some(query) = arguments.get("query").and_then(Value::as_str) else {
                    return Err("sql 要给 query：一条 SELECT 语句（表结构见 describe）".to_owned());
                };
                let snapshot = self.snapshots.latest();
                let position = &snapshot.self_state.position;
                return crate::sql::run(
                    &memory,
                    [position.x, position.y, position.z],
                    ALIASES,
                    query,
                );
            }
            Some("describe") => return Ok(crate::sql::SCHEMA_DOC.to_owned()),
            _ => return Err("action 必须是 find/around/sql/describe 之一；请改写调用".to_owned()),
        };
        Ok(format!(
            "{body}\n（这些是你看过的；记忆里一共 {} 格）",
            memory.len()
        ))
    }
}

pub(crate) fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["find", "around", "sql", "describe"],
                "description": "find=找某类方块在哪（要 what）；around=看看身边记住了些什么；sql=用一条 SELECT 自己查（要 query，表结构见 describe）；describe=取表结构与例子"
            },
            "query": { "type": "string", "description": "sql 用：一条只读 SELECT。表是 seen_blocks（x,y,z,name,label,distance,props）与 block_aliases（alias,pattern）" },
            "what": {
                "type": "string",
                "description": "find 用：找什么。可用「木头/树叶/石头/矿石/铁/煤/水/岩浆/沙子/土/容器/炉子/工作台/床/门」，也可以直接写方块名（如 spruce_log）"
            },
            "range": { "type": "integer", "description": "可选：只看这么多格以内（around 默认 16，find 默认不限）" },
            "limit": { "type": "integer", "description": "可选：最多给几处（默认 12，上限 64）" }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

pub(crate) const DESCRIPTION: &str = "\
查你自己的方块记忆库——**只查得到你看过的东西**，没看过的地方它一无所知（先 scan）。\
find=某类方块在哪（如 what=「木头」「铁」）；around=身边记住了些什么。\
答案是坐标与方位距离，怎么解读由你自己判断。不打断任何动作。";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_expand_to_block_names() {
        assert_eq!(expand("木头"), vec!["_log", "_wood", "_stem"]);
        // 不认得的词按方块名直用，模型可以直接写注册名。
        assert_eq!(expand("Spruce_Log"), vec!["spruce_log"]);
    }

    #[test]
    fn matching_ignores_the_namespace() {
        let needles = expand("木头");
        assert!(matches("minecraft:spruce_log", &needles));
        assert!(!matches("minecraft:stone", &needles));
    }

    /// 距离是三维的：正上方 5 格比水平 3 格远。
    #[test]
    fn distance_counts_height() {
        assert!(squared([0, 0, 0], [0, 5, 0]) > squared([0, 0, 0], [3, 0, 0]));
    }
}

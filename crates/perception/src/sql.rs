//! 方块记忆的 SQL 查询面。
//!
//! # 为什么给 SQL
//!
//! 记忆本身没有上限（`BlockMemory` 是一张裸表，不淘汰不过期），查询侧却只有
//! `find`/`around` 两个写死的动作加一张手维护的别名表。「y 小于 40 的铁矿按
//! 距离排序」「我看过的方块各有多少」这类问题，工具面根本没有语法去表达它们。
//!
//! 给 SQL 不是"更强的检索"，是把**提问的自由**还给模型：它写得出的问题，
//! 不该被我们的动作表挡住。
//!
//! # 为什么这不越界
//!
//! 库里只有观察过的方块，SQL 查不出没看过的东西——[`crate::blocks`] 那条
//! 信息边界原样成立。聚合与精确坐标召回确实超出人类，但那是**补偿**：模型
//! 没有连续视觉，形不成「出生在悬崖上、旁边是丛林」这种整体理解，用超人的
//! 坐标记忆补这个缺口，整体仍在正常玩家的水平附近。
//!
//! # 为什么不把热存储搬进 SQLite
//!
//! `BlockMemory` 是**寻路器每 tick 读、组合根每 250ms 写**的热路径。把它换成
//! SQLite 是拿移动和感知去冒险。这里改为**查询时现物化**：几千行毫秒级，
//! 五十万行也不过零点几秒，而 SQLite 只活在「模型发问」这一条路径上。
//!
//! # 三道闸，缺一不可
//!
//! 输出条数管的是产出，管不了计算——`WITH RECURSIVE` 可以只返回一行然后跑到
//! 天荒地老。所以三道一起上：白名单授权器（不是黑名单）、墙钟预算、
//! 行数与字节数双上限。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, Row};
use world::BlockMemory;

/// 结果条数上限。再多就回到了「逐格推送」那条死路。
const MAX_ROWS: usize = 200;
/// 输出字节上限。行数少但每行极长（比如 `SELECT group_concat(...)`）同样要挡。
const MAX_BYTES: usize = 8 * 1024;
/// 单次查询的墙钟预算。超了中断，如实说超了。
const TIME_BUDGET: Duration = Duration::from_millis(300);
/// 进度回调的检查粒度（虚拟机指令数）。小到能及时中断，大到不拖慢正常查询。
const PROGRESS_OPS: i32 = 10_000;

/// 模型看得见的两张表。授权器只认这两个名字。
const SEEN_BLOCKS: &str = "seen_blocks";
const BLOCK_ALIASES: &str = "block_aliases";

/// 表结构说明。随 `describe` 给出，不进常驻工具描述——静态文本每次都发一遍
/// 是纯浪费，而它一个字都不会变。
pub(crate) const SCHEMA_DOC: &str = r#"你的方块记忆是一张可以用 SQL 查的表。只读，一次一条 SELECT。

seen_blocks —— 你**亲眼看见过**的方块，每格一行
  x, y, z     整数坐标
  name        方块名，去掉了 minecraft: 前缀（如 iron_ore、jungle_log）
  label       带可见属性的完整标签（如 jungle_log[axis=y]），和清单里显示的一致
  distance    离你现在的位置多少格（已经算好，可以直接 ORDER BY）
  props       属性的 JSON，可用 json_extract(props,'$.axis')

block_aliases —— 说法到方块名的对照，可以 JOIN，也可以不用
  alias       说法，如「木头」「矿石」
  pattern     该说法涵盖的名字片段，如 _log、_ore

例子：
  SELECT name, x, y, z, distance FROM seen_blocks
   WHERE name LIKE '%iron_ore%' ORDER BY distance LIMIT 5
  SELECT name, COUNT(*) n FROM seen_blocks GROUP BY name ORDER BY n DESC LIMIT 10
  SELECT b.name, b.x, b.y, b.z FROM seen_blocks b
    JOIN block_aliases a ON b.name LIKE '%'||a.pattern||'%'
   WHERE a.alias='矿石' AND b.y < 40 ORDER BY b.distance

**这张表只装你看见过、而且当时露出面的方块。** 埋在石头里的矿脉从来不会
出现在这里——查不到不等于附近没有，那种只能挖开才知道。"#;

/// 一次查询的结果：表格文本 + 是否被上限截断。
pub(crate) fn run(
    memory: &BlockMemory,
    origin: [f64; 3],
    aliases: &[(&str, &[&str])],
    query: &str,
) -> Result<String, String> {
    let trimmed = query.trim().trim_end_matches(';');
    if trimmed.is_empty() {
        return Err("query 是空的：给一条 SELECT 语句".to_owned());
    }
    // 多条语句会让「只读」的判断失去意义（第二条可以是任何东西）。
    // 授权器也挡得住，但在这里先说清楚，错因更好懂。
    if trimmed.contains(';') {
        return Err("一次只能查一条语句；把分号后面那部分单独发".to_owned());
    }

    let connection = Connection::open_in_memory().map_err(|error| format!("开库失败：{error}"))?;
    materialize(&connection, memory, origin, aliases)?;

    // query_only 要在装授权器之前设——装上之后 PRAGMA 就被拒了。
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|error| format!("设只读失败：{error}"))?;
    install_authorizer(&connection)?;

    let deadline = Instant::now() + TIME_BUDGET;
    let timed_out = Arc::new(AtomicBool::new(false));
    let flag = timed_out.clone();
    connection
        .progress_handler(
            PROGRESS_OPS,
            Some(move || {
                if Instant::now() >= deadline {
                    flag.store(true, Ordering::Relaxed);
                    return true; // 中断
                }
                false
            }),
        )
        .map_err(|error| format!("装进度回调失败：{error}"))?;

    let rendered = select(&connection, trimmed);
    match rendered {
        Ok(text) => Ok(text),
        Err(error) if timed_out.load(Ordering::Relaxed) => Err(format!(
            "这条查询跑了超过 {} 毫秒，被中断了——换个窄一点的写法（加 WHERE、加 LIMIT）。（{error}）",
            TIME_BUDGET.as_millis()
        )),
        Err(error) => Err(error),
    }
}

/// 把记忆物化成两张表。距离在这里算好，模型不用自己开方。
fn materialize(
    connection: &Connection,
    memory: &BlockMemory,
    origin: [f64; 3],
    aliases: &[(&str, &[&str])],
) -> Result<(), String> {
    connection
        .execute_batch(&format!(
            "CREATE TABLE {SEEN_BLOCKS}(
                 x INTEGER, y INTEGER, z INTEGER,
                 name TEXT, label TEXT, distance REAL, props TEXT);
             CREATE TABLE {BLOCK_ALIASES}(alias TEXT, pattern TEXT);"
        ))
        .map_err(|error| format!("建表失败：{error}"))?;

    {
        let mut insert = connection
            .prepare(&format!(
                "INSERT INTO {SEEN_BLOCKS} VALUES (?1,?2,?3,?4,?5,?6,?7)"
            ))
            .map_err(|error| format!("准备插入失败：{error}"))?;
        for (at, fact) in memory.iter() {
            let bare = fact.name.strip_prefix("minecraft:").unwrap_or(&fact.name);
            let dx = f64::from(at[0]) + 0.5 - origin[0];
            let dy = f64::from(at[1]) + 0.5 - origin[1];
            let dz = f64::from(at[2]) + 0.5 - origin[2];
            let distance = (dx * dx + dy * dy + dz * dz).sqrt();
            let props = serde_json::to_string(&fact.properties).unwrap_or_else(|_| "{}".to_owned());
            insert
                .execute(rusqlite::params![
                    at[0],
                    at[1],
                    at[2],
                    bare,
                    world::visible_block_label(&fact.name, &fact.properties),
                    distance,
                    props,
                ])
                .map_err(|error| format!("写入失败：{error}"))?;
        }
    }
    {
        let mut insert = connection
            .prepare(&format!("INSERT INTO {BLOCK_ALIASES} VALUES (?1,?2)"))
            .map_err(|error| format!("准备插入失败：{error}"))?;
        for (alias, patterns) in aliases {
            for pattern in *patterns {
                insert
                    .execute(rusqlite::params![alias, pattern])
                    .map_err(|error| format!("写入失败：{error}"))?;
            }
        }
    }
    connection
        .execute_batch(&format!(
            "CREATE INDEX seen_name ON {SEEN_BLOCKS}(name);
             CREATE INDEX seen_distance ON {SEEN_BLOCKS}(distance);"
        ))
        .map_err(|error| format!("建索引失败：{error}"))
}

/// 授权器：挡住**写入与逃逸**，不去挡读。
///
/// 按表名筛读是个错的安全模型。这个库每次查询现建，里面**只有我们放进去的
/// 那两张表**——信息边界由「我们放了什么」保证，不由授权器筛表名。按表名筛
/// 反而会误伤：`WITH x AS (...) SELECT * FROM x` 里的 `x` 在授权器眼里也是
/// 一张表，一刀切下去连普通 CTE 都用不了。
///
/// 放行的四类之外**一律 Deny**，不是列黑名单：`AuthAction` 是
/// `non_exhaustive` 的，上游加一个变体，黑名单就漏一个，白名单则最多误伤。
fn install_authorizer(connection: &Connection) -> Result<(), String> {
    connection
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Select | AuthAction::Read { .. } => Authorization::Allow,
            // 函数放行，只拦装扩展——那是唯一能把任意代码带进来的口子。
            AuthAction::Function { function_name } => {
                if function_name.eq_ignore_ascii_case("load_extension") {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }
            // 递归 CTE 留着：表达力的一大块，跑飞的风险由墙钟预算兜。
            AuthAction::Recursive => Authorization::Allow,
            _ => Authorization::Deny,
        }))
        .map_err(|error| format!("装授权器失败：{error}"))
}

/// 跑查询并渲染成表格。行数与字节数两道上限，任一触顶都如实说。
fn select(connection: &Connection, query: &str) -> Result<String, String> {
    let mut statement = connection.prepare(query).map_err(explain)?;
    let columns: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if columns.is_empty() {
        return Err("这条语句没有返回任何列；SQL 查询要是 SELECT".to_owned());
    }

    let mut rows = statement.query([]).map_err(explain)?;
    let mut lines = vec![columns.join(" | ")];
    let mut bytes = lines[0].len();
    let mut count = 0usize;
    let mut truncated = None;
    while let Some(row) = rows.next().map_err(explain)? {
        if count >= MAX_ROWS {
            truncated = Some(format!("超过 {MAX_ROWS} 行，只给了前 {MAX_ROWS} 行"));
            break;
        }
        let line = render_row(row, columns.len());
        if bytes + line.len() > MAX_BYTES {
            truncated = Some(format!("输出超过 {} KB，就到这里", MAX_BYTES / 1024));
            break;
        }
        bytes += line.len();
        lines.push(line);
        count += 1;
    }

    if count == 0 {
        return Ok("查到 0 行。".to_owned());
    }
    let mut text = lines.join("\n");
    text.push_str(&format!("\n（{count} 行"));
    if let Some(note) = truncated {
        text.push('；');
        text.push_str(&note);
    }
    text.push('）');
    Ok(text)
}

fn render_row(row: &Row<'_>, columns: usize) -> String {
    (0..columns)
        .map(|index| match row.get_ref(index) {
            Ok(rusqlite::types::ValueRef::Null) => "NULL".to_owned(),
            Ok(rusqlite::types::ValueRef::Integer(value)) => value.to_string(),
            // 距离这类浮点保留一位：模型要的是「多远」，不是十七位有效数字。
            Ok(rusqlite::types::ValueRef::Real(value)) => format!("{value:.1}"),
            Ok(rusqlite::types::ValueRef::Text(bytes)) => {
                String::from_utf8_lossy(bytes).into_owned()
            }
            Ok(rusqlite::types::ValueRef::Blob(bytes)) => format!("<{} 字节>", bytes.len()),
            Err(_) => "?".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 把 SQLite 的报错翻成模型能据以改写的话。
///
/// 被授权器拒掉时 SQLite 只说 "not authorized"，不说拒的是什么——那句话
/// 对模型毫无用处，它会以为语法错了。这里补上真正的边界。
fn explain(error: rusqlite::Error) -> String {
    let text = error.to_string();
    if text.contains("not authorized") {
        return format!(
            "这条查询碰了不让碰的东西：只读、一次一条 SELECT，只能读 {SEEN_BLOCKS} 与 \
{BLOCK_ALIASES} 两张表。（{text}）"
        );
    }
    if text.contains("interrupted") {
        return format!("查询被中断（多半是跑太久）。（{text}）");
    }
    format!("SQL 报错：{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALIASES: &[(&str, &[&str])] = &[("矿石", &["_ore"]), ("木头", &["_log"])];

    fn block(
        at: [i32; 3],
        name: &str,
        properties: std::collections::BTreeMap<String, String>,
    ) -> world::ViewportBlock {
        world::ViewportBlock {
            name: name.to_owned(),
            properties,
            position: at,
        }
    }

    /// 三格：脚边一块铁矿、远处一块原木、更远一块石头。
    fn memory() -> BlockMemory {
        let mut axis = std::collections::BTreeMap::new();
        axis.insert("axis".to_owned(), "y".to_owned());
        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[
            block([1, 64, 0], "minecraft:iron_ore", Default::default()),
            block([10, 64, 0], "minecraft:jungle_log", axis),
            block([40, 64, 0], "minecraft:stone", Default::default()),
        ]);
        memory
    }

    fn ask(query: &str) -> Result<String, String> {
        run(&memory(), [0.5, 64.5, 0.5], ALIASES, query)
    }

    #[test]
    fn basic_select_answers_with_addresses_and_distance() {
        let text =
            ask("SELECT name, x, y, z, distance FROM seen_blocks ORDER BY distance").unwrap();
        assert!(text.contains("iron_ore | 1 | 64 | 0 | 1.0"), "{text}");
        // 距离按一位小数给：模型要的是「多远」，不是十七位有效数字。
        assert!(!text.contains("1.0000000"), "{text}");
        assert!(text.contains("（3 行）"), "{text}");
    }

    /// 别名表是可 JOIN 的数据，不是唯一入口——策展知识留着，表达力不被它堵死。
    #[test]
    fn aliases_are_joinable() {
        let text = ask("SELECT b.name FROM seen_blocks b JOIN block_aliases a \
             ON b.name LIKE '%'||a.pattern||'%' WHERE a.alias='矿石'")
        .unwrap();
        assert!(text.contains("iron_ore"), "{text}");
        assert!(!text.contains("stone"), "{text}");
    }

    #[test]
    fn props_are_queryable_as_json() {
        let text =
            ask("SELECT json_extract(props,'$.axis') FROM seen_blocks WHERE name='jungle_log'")
                .unwrap();
        assert!(text.contains('y'), "{text}");
    }

    #[test]
    fn empty_result_says_so_plainly() {
        assert_eq!(
            ask("SELECT * FROM seen_blocks WHERE name='diamond_ore'").unwrap(),
            "查到 0 行。"
        );
    }

    /// 写操作一律拒。授权器是**白名单**：不在放行名单里的动作全部落到 Deny，
    /// 所以上游给 `AuthAction` 加变体也不会漏。
    #[test]
    fn every_kind_of_write_is_refused() {
        for query in [
            "DELETE FROM seen_blocks",
            "UPDATE seen_blocks SET name='gold_ore'",
            "INSERT INTO seen_blocks VALUES (0,0,0,'x','x',0,'{}')",
            "DROP TABLE seen_blocks",
            "CREATE TABLE evil(x)",
            "ALTER TABLE seen_blocks RENAME TO evil",
        ] {
            let outcome = ask(query);
            assert!(
                outcome.is_err(),
                "写操作居然被放行了：{query} → {outcome:?}"
            );
        }
    }

    /// 越权读取：库外的表、附加别的库、改 PRAGMA、装扩展。
    #[test]
    fn reaching_outside_the_two_tables_is_refused() {
        for query in [
            "ATTACH DATABASE 'other.db' AS other",
            "SELECT load_extension('evil.so')",
            "PRAGMA journal_mode = WAL",
        ] {
            let outcome = ask(query);
            assert!(outcome.is_err(), "越权居然被放行了：{query} → {outcome:?}");
        }
    }

    /// 拒绝话术要说清边界。SQLite 原话只说 "not authorized"，
    /// 模型会以为是语法错，然后改一版同样越权的写法再试。
    #[test]
    fn refusal_names_the_boundary_instead_of_just_saying_not_authorized() {
        let error = ask("DELETE FROM seen_blocks").unwrap_err();
        assert!(error.contains("seen_blocks"), "{error}");
        assert!(error.contains("只读"), "{error}");
    }

    /// 多条语句：第二条可以是任何东西，「只读」的判断会失去意义。
    #[test]
    fn only_one_statement_per_call() {
        let error = ask("SELECT 1 FROM seen_blocks; DELETE FROM seen_blocks").unwrap_err();
        assert!(error.contains("一次只能查一条"), "{error}");
        // 末尾的分号是习惯写法，不算第二条。
        assert!(ask("SELECT name FROM seen_blocks;").is_ok());
    }

    /// 行数上限。**这一道管产出，不管计算**——下一个测试管计算。
    #[test]
    fn row_cap_truncates_and_says_so() {
        let mut memory = BlockMemory::new();
        let many: Vec<world::ViewportBlock> = (0..(MAX_ROWS as i32 + 50))
            .map(|index| block([index, 64, 0], "minecraft:stone", Default::default()))
            .collect();
        memory.absorb_visible(&many);
        let text = run(
            &memory,
            [0.0, 0.0, 0.0],
            ALIASES,
            "SELECT x FROM seen_blocks",
        )
        .unwrap();
        assert!(text.contains(&format!("超过 {MAX_ROWS} 行")), "{text}");
        assert_eq!(text.lines().count(), MAX_ROWS + 2); // 表头 + 行 + 尾注
    }

    /// 计算上限：递归 CTE 只返回一行却可以永远跑下去，行数上限完全挡不住。
    /// 这正是「输出上限不够，必须配墙钟预算」的那一类。
    #[test]
    fn a_runaway_query_is_interrupted_by_the_time_budget() {
        let at = std::time::Instant::now();
        let error = ask(
            "WITH RECURSIVE forever(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM forever) \
             SELECT count(*) FROM forever",
        )
        .unwrap_err();
        assert!(error.contains("中断"), "{error}");
        // 预算是 300ms，给足余量也不该拖到几秒。
        assert!(
            at.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            at.elapsed()
        );
    }

    /// CTE 必须可用。按表名筛读会把 `WITH x AS (...) SELECT * FROM x` 里的
    /// `x` 当成越权表——这个测试钉住「不按表名筛」这个决定。
    #[test]
    fn common_table_expressions_work() {
        let text = ask(
            "WITH ores AS (SELECT * FROM seen_blocks WHERE name LIKE '%_ore%')              SELECT name FROM ores",
        )
        .unwrap();
        assert!(text.contains("iron_ore"), "{text}");
    }

    #[test]
    fn empty_query_is_refused_with_a_usable_message() {
        assert!(ask("   ").unwrap_err().contains("SELECT"));
    }
}

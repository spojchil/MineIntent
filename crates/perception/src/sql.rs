//! 方块记忆的 SQL 查询面。
//!
//! # 为什么给 SQL
//!
//! 记忆本身没有上限（不淘汰、不过期），查询侧却只有 `find`/`around` 两个写死
//! 的动作加一张手维护的别名表。「y 小于 40 的铁矿按
//! 距离排序」「我看过的方块各有多少」这类问题，工具面根本没有语法去表达它们。
//!
//! 给 SQL 不是"更强的检索"，是把**提问的自由**还给模型：它写得出的问题，
//! 不该被我们的动作表挡住。
//!
//! # 表是记忆的纯函数
//!
//! 位置不进 `seen_blocks`。带一列算好的距离看着方便，代价是这张表变成
//! `(记忆, 当前位置)` 的函数，而且把「当下的位置」和「陈旧的观察」拼进同一行，
//! 整张表读起来像实时快照。现在位置单独占 [`ME`] 那一行，要用得显式跨过去。
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
//! SQLite 是拿移动和感知去冒险。世界那两张表因此是**虚表**（见 [`crate::vtab`]），
//! 底下还是那本记忆，SQLite 只活在「模型发问」这一条路径上，一行都不产生。
//!
//! 而且读的是一份**快照**：查询期一律不持记忆的锁，否则 300ms 的预算会把眼睛
//! 和寻路器一起按住，同一条 SQL 里还会看到前后不一致的世界。
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

use crate::vtab;

/// 结果条数上限。再多就回到了「逐格推送」那条死路。
const MAX_ROWS: usize = 200;
/// 输出字节上限。行数少但每行极长（比如 `SELECT group_concat(...)`）同样要挡。
const MAX_BYTES: usize = 8 * 1024;
/// 单次查询的墙钟预算。超了中断，如实说超了。
const TIME_BUDGET: Duration = Duration::from_millis(300);
/// 进度回调的检查粒度（虚拟机指令数）。小到能及时中断，大到不拖慢正常查询。
const PROGRESS_OPS: i32 = 10_000;

/// 模型看得见的表。世界那两张是虚表（见 [`crate::vtab`]），`me` 现建。
const SEEN_BLOCKS: &str = "seen_blocks";
const SEEN_EMPTY: &str = "seen_empty";
/// 此时此地：一行，把「当下」从那张陈旧的表里剥出来单独放。
const ME: &str = "me";

/// 表结构说明。**并进常驻工具描述**，不做成按需拉取的动作。
///
/// 工具定义走的是请求的 `tools` 字段：每次请求发一份、不累积，落在提示前缀里，
/// 缓存吃得住。做成 `describe` 动作看似省了，实际是——模型第一次用 SQL 之前
/// 要多花一次往返，拿到之后这段文字**从此每次请求都跟着转录发**，一次都没省;
/// 而且它落在可压缩区，会在会话中途被压掉，模型手里的语法参考凭空消失，
/// 它还不会知道自己丢了。表结构一个字都不会变，没有「按需」的前提。
pub(crate) const SCHEMA_DOC: &str = r#"你的方块记忆可以用 SQL 查。只读，一次一条 SELECT。

三张表：

seen_blocks —— 看过、**有东西**的格，每格一行
  x, y, z     整数坐标
  name        方块名（如 iron_ore、jungle_log）
  label       带可见属性的完整标签（如 jungle_log[axis=y]）
  props       属性的 JSON，可用 json_extract(props,'$.axis')

seen_empty —— 看过、**是空的**格，每格一行
  x, y, z     整数坐标

me —— 你此刻在哪，只有一行
  x, y, z     当前位置（小数）

函数：dist(x,y,z) 那一格离你多少格。标准数学函数也在：sqrt、floor、ceil、pow、mod。
把小数坐标（me 那三列）对回方块格要用 floor，不能用 CAST——负坐标上会差一格。

**没看过 = 两张表都没有。** 它没有自己的表，因为世界无界、补集数不完；
要问就写反连接，而且自己圈好坐标范围：
  SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM seen_blocks WHERE x=10 AND y=64 AND z=3)
              AND NOT EXISTS (SELECT 1 FROM seen_empty  WHERE x=10 AND y=64 AND z=3)

**两张表「查不到」的原因不一样，别拿一张去推另一张：**
seen_blocks 只装当时**露出面**的方块——埋在石头里的矿脉从来不进来，
查不到不等于附近没有，那种只能挖开才知道。
seen_empty 只装**射线穿过**的空格——看天的方向、玻璃水树叶身后不进来，
查不到不等于那里有东西，多半只是还没看过。

例子：
  SELECT name, x, y, z, dist(x,y,z) d FROM seen_blocks
   WHERE name LIKE '%iron_ore%' ORDER BY d LIMIT 5
  SELECT name, COUNT(*) n FROM seen_blocks GROUP BY name ORDER BY n DESC LIMIT 10
  -- 一类方块：名字片段自己写，你知道哪些名字属于哪一类
  SELECT name, x, y, z FROM seen_blocks
   WHERE (name LIKE '%_ore' OR name LIKE '%_log') AND y < 40
   ORDER BY dist(x,y,z) LIMIT 10
  -- 站得住脚的位置：看过的方块，头顶两格确认为空
  SELECT b.x, b.y+1 AS y, b.z FROM seen_blocks b
    JOIN seen_empty a ON a.x=b.x AND a.y=b.y+1 AND a.z=b.z
    JOIN seen_empty c ON c.x=b.x AND c.y=b.y+2 AND c.z=b.z
   ORDER BY dist(b.x,b.y,b.z) LIMIT 10
  SELECT name, x, y, z FROM seen_blocks, me WHERE y > me.y + 3

查得太宽会被行数上限或时间预算截断，如实告诉你；那时把范围收窄（加 WHERE、
加坐标范围、加 LIMIT），不要重发同一条。"#;

/// 一次查询的结果：表格文本 + 是否被上限截断。
pub(crate) fn run(
    memory: Arc<BlockMemory>,
    origin: [f64; 3],
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
    vtab::load_modules(&connection, memory).map_err(|error| format!("装虚表失败：{error}"))?;
    materialize(&connection, origin)?;
    install_functions(&connection, origin)?;

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

/// 建唯一需要物化的小表：此时此地。
///
/// **世界那两张不在这里**——`seen_blocks` 与 `seen_empty` 是虚表，一行都不产生。
/// 剩下的只有 `me` 一行，现建的代价可以忽略。
fn materialize(connection: &Connection, origin: [f64; 3]) -> Result<(), String> {
    connection
        .execute_batch(&format!("CREATE TABLE {ME}(x REAL, y REAL, z REAL);"))
        .map_err(|error| format!("建表失败：{error}"))?;

    connection
        .execute(
            &format!("INSERT INTO {ME} VALUES (?1,?2,?3)"),
            rusqlite::params![origin[0], origin[1], origin[2]],
        )
        .map_err(|error| format!("写入失败：{error}"))?;
    Ok(())
}

/// 补一个标量函数。
///
/// 只补一个：数学函数那一整批（`sqrt` `floor` `ceil` `pow` `mod` …）由
/// `.cargo/config.toml` 里的 `LIBSQLITE3_FLAGS` 开出来，不在这里手写。手写垫片
/// 要求我们预判哪些函数不存在，而这件事已经证明预判不了——所以整套开齐，用
/// [`tests::math_functions_are_compiled_in`] 那条哨兵守住。
///
/// `dist` 不属于那一批：它补的不是数学，是两件本地知识。有了它，[`SEEN_BLOCKS`] 就不必带 `distance`
/// 列：那一列会让表变成 `(记忆, 位置)` 的函数而不是记忆的函数，而且把「当下的
/// 位置」和「陈旧的观察」拼进同一行。现在距离按需现算，只对查询真正碰到的行
/// 付钱，位置则单独摆在 [`ME`] 那一行里，要用得显式跨过去。
///
/// 坐标按方块角算（`+0.5` 取格心），与格坐标本身的含义一致。
fn install_functions(connection: &Connection, origin: [f64; 3]) -> Result<(), String> {
    use rusqlite::functions::FunctionFlags;
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    connection
        .create_scalar_function("dist", 3, flags, move |context| {
            let dx = context.get::<f64>(0)? + 0.5 - origin[0];
            let dy = context.get::<f64>(1)? + 0.5 - origin[1];
            let dz = context.get::<f64>(2)? + 0.5 - origin[2];
            Ok((dx * dx + dy * dy + dz * dz).sqrt())
        })
        .map_err(|error| format!("装 dist 失败：{error}"))
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
            "这条查询碰了不让碰的东西：只读、一次一条 SELECT，只能读 {SEEN_BLOCKS}、\
{SEEN_EMPTY} 与 {ME}。（{text}）"
        );
    }
    if text.contains("may not be modified") {
        return format!("这张表只能读，不能改：只读，一次一条 SELECT。（{text}）");
    }
    if text.contains("interrupted") {
        return format!("查询被中断（多半是跑太久）。（{text}）");
    }
    format!("SQL 报错：{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        memory.absorb_visible(
            &[
                block([1, 64, 0], "iron_ore", Default::default()),
                block([10, 64, 0], "jungle_log", axis),
                block([40, 64, 0], "stone", Default::default()),
            ],
            0,
        );
        memory
    }

    fn ask(query: &str) -> Result<String, String> {
        run(Arc::new(memory()), [0.5, 64.5, 0.5], query)
    }

    #[test]
    fn basic_select_answers_with_addresses_and_distance() {
        let text = ask("SELECT name, x, y, z, dist(x,y,z) d FROM seen_blocks ORDER BY d").unwrap();
        assert!(text.contains("iron_ore | 1 | 64 | 0 | 1.0"), "{text}");
        // 距离按一位小数给：模型要的是「多远」，不是十七位有效数字。
        assert!(!text.contains("1.0000000"), "{text}");
        assert!(text.contains("（3 行）"), "{text}");
    }

    /// 数学函数靠 `.cargo/config.toml` 里的构建期开关，没设也能编译通过，洞只在
    /// 模型发出查询时才露出来。这条把静默的运行时缺函数变成红色的测试。
    ///
    /// **哨兵是 `floor` 不是 `sqrt`**：`dist` 之外我们不再注册任何东西，但历史上
    /// 手写过 `sqrt`，而手写的会覆盖内建的同名函数——拿它当哨兵，开关丢了也照样绿。
    /// 而 `floor` 正是没有它就会静默错一格的那个：`CAST(-3.5 AS INTEGER)` 和
    /// `-7/2` 都向零截断，负坐标上答案差一格且不报错。
    #[test]
    fn math_functions_are_compiled_in() {
        let text = ask("SELECT floor(-3.5) AS f").unwrap();
        assert!(
            text.contains("-4"),
            "floor 不在，检查 LIBSQLITE3_FLAGS：{text}"
        );
        for call in ["sqrt(100.0)", "ceil(1.2)", "pow(2.0,3.0)", "mod(7.0,3.0)"] {
            ask(&format!("SELECT {call}"))
                .unwrap_or_else(|error| panic!("{call} 不在，检查 LIBSQLITE3_FLAGS：{error}"));
        }
    }

    /// 距离不再是表上的一列：表是记忆的纯函数，位置单独摆在 me 那一行。
    #[test]
    fn the_table_carries_no_position_derived_column() {
        let error = ask("SELECT distance FROM seen_blocks").unwrap_err();
        assert!(error.contains("distance"), "{error}");
        let text = ask("SELECT x, y, z FROM me").unwrap();
        assert!(text.contains("0.5 | 64.5 | 0.5"), "{text}");
    }

    /// 三格方块之外再记一片确认为空的空间。
    ///
    /// 规模差异**就是**规划器选边的依据：三格对三格的玩具数据里，全扫和点探
    /// 一样便宜，计划好坏根本看不出来。这里 5120 格空 对 3 格方块。
    fn memory_with_space() -> BlockMemory {
        let mut memory = memory();
        for x in 0..16 {
            for y in 65..85 {
                for z in 0..16 {
                    memory.observe([x, y, z], None, 0);
                }
            }
        }
        memory
    }

    fn ask_in_space(query: &str) -> Result<String, String> {
        run(Arc::new(memory_with_space()), [0.5, 64.5, 0.5], query)
    }

    /// **xBestIndex 决定的是量级，不是常数。**
    ///
    /// 「哪些方块头顶两格是空的」这条自连接，两种计划差着数量级：方块表驱动、
    /// 空气表内层是每行两次位测试；反过来是几千格全扫再回探。选哪个完全由我们
    /// 报的代价决定，报错了不会有任何报错，只会慢——所以钉的是**计划**，不是
    /// 耗时（计时断言既不稳又不指人）。
    ///
    /// `INDEX 7` = 计划位 0b111 = 三个轴全是等值约束，也就是走了点探那一档。
    #[test]
    fn the_planner_probes_the_empty_table_instead_of_scanning_it() {
        let text = ask_in_space(
            "EXPLAIN QUERY PLAN SELECT b.name FROM seen_blocks b \
             JOIN seen_empty e ON e.x=b.x AND e.y=b.y+1 AND e.z=b.z",
        )
        .unwrap();
        assert!(text.contains("INDEX 7"), "空气表没走点探：{text}");
    }

    /// 联表查询才是给第二张表的理由：这条问的是「哪些看过的方块，头顶两格
    /// 确认为空」——也就是站得住脚的位置，用 SQL 声明了一遍寻路的合法性谓词。
    #[test]
    fn standable_spots_fall_out_of_joining_the_two_tables() {
        let text = ask_in_space(
            "SELECT b.x, b.y, b.z FROM seen_blocks b \
             JOIN seen_empty a ON a.x=b.x AND a.y=b.y+1 AND a.z=b.z \
             JOIN seen_empty c ON c.x=b.x AND c.y=b.y+2 AND c.z=b.z \
             ORDER BY dist(b.x,b.y,b.z)",
        )
        .unwrap();
        assert!(text.contains("1 | 64 | 0"), "{text}");
        assert!(text.contains("10 | 64 | 0"), "{text}");
        // 40 号那块在记过的空间之外，头顶是「没看过」而不是「确认为空」。
        assert!(!text.contains("40 | 64 | 0"), "{text}");
    }

    /// 三态在 SQL 面上的样子：有东西是一张表的行，确认为空是另一张表的行，
    /// 没看过是**两张都没有**——补集无界，只能反连接，不能有自己的表。
    #[test]
    fn the_three_states_read_out_as_two_tables_and_a_gap() {
        let there =
            ask_in_space("SELECT name FROM seen_blocks WHERE x=1 AND y=64 AND z=0").unwrap();
        assert!(there.contains("iron_ore"), "{there}");

        let empty = ask_in_space("SELECT x FROM seen_empty WHERE x=1 AND y=65 AND z=0").unwrap();
        assert!(empty.contains("（1 行）"), "{empty}");

        let unseen = ask_in_space(
            "SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM seen_blocks WHERE x=999 AND y=64 AND z=999) \
               AND NOT EXISTS (SELECT 1 FROM seen_empty WHERE x=999 AND y=64 AND z=999)",
        )
        .unwrap();
        assert!(
            unseen.contains("（1 行）"),
            "没看过应当两张表都查不到：{unseen}"
        );
    }

    /// 空气表全扫会撞行数上限——不是缺陷，是「别问这么宽的问题」该有的回答。
    #[test]
    fn scanning_all_of_the_empty_space_hits_the_row_cap() {
        let text = ask_in_space("SELECT x, y, z FROM seen_empty").unwrap();
        assert!(text.contains(&format!("超过 {MAX_ROWS} 行")), "{text}");
    }

    /// 词表删掉之后，「找矿石」这类问题照样问得出来——名字片段由模型自己写。
    /// 这条钉的是删除的前提：翻译能力没有消失，只是搬回了模型的常识里。
    #[test]
    fn a_category_question_still_works_without_any_alias_table() {
        let text = ask("SELECT name FROM seen_blocks WHERE name LIKE '%_ore'").unwrap();
        assert!(text.contains("iron_ore"), "{text}");
        assert!(!text.contains("stone"), "{text}");
    }

    /// 词表真的不在了：查它应当报错，而不是悄悄返回空集。
    #[test]
    fn the_alias_table_is_gone_and_says_so() {
        let error = ask("SELECT * FROM block_aliases").unwrap_err();
        // 话术不复述表名清单：表结构在常驻工具描述里，每次请求都跟着发，
        // 模型手上本来就有，错误里再抄一遍是冗余。
        assert!(error.contains("block_aliases"), "{error}");
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
        let error = ask("INSERT INTO seen_empty VALUES (0,0,0)").unwrap_err();
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
            .map(|index| block([index, 64, 0], "stone", Default::default()))
            .collect();
        memory.absorb_visible(&many, 0);
        let text = run(
            Arc::new(memory),
            [0.0, 0.0, 0.0],
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

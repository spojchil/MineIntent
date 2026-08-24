//! 方块记忆的两张虚表：`seen_blocks` 与 `seen_empty`。
//!
//! # 为什么是虚表而不是物化
//!
//! 「看过有东西」和「看过是空的」是同一张关系的两支，模型面上给两张表是因为
//! 规模差三个数量级（分开之后这个差异对规划器和对模型都是显式的，忘写条件也
//! 不会串味）。空的那一支根本没法物化——一次视野的自由体积就是数万格。
//!
//! 有东西那一支物化过，代价是每次查询重建全部行加两个索引，而记忆不淘汰、
//! 只增不减，那笔重建费只会涨。虚表把它变成按需读：`xColumn` 只对查询真正
//! 要的列调用，`SELECT name,x,y,z` 一次 `props` 的 JSON 都不会生成。
//!
//! # xBestIndex 决定的是量级，不是常数
//!
//! 「哪些格上方两格是空的」这种自连接，两种计划差得离谱：`seen_blocks` 驱动、
//! `seen_empty` 内层是每行两次位测试；反过来是几十万格全扫。选哪个**完全由
//! 我们报的代价决定**，报错了不会有任何报错，只会慢。
//!
//! 所以两张表共用同一套计划编码与代价口径（[`plan`]）——键相同、分片相同，
//! 代价模型也就对称，规划器不会因为一边报 O(1)、一边报全扫而选歪。全扫那一档
//! **必须报大**，否则规划器会把它拎到外层。
//!
//! # 游标的两条硬约束
//!
//! **一、点探要真的是 O(1)。** 三维全等值时直接问记忆那一格，不走区段遍历
//! ——那是 JOIN 里最热的路径。
//!
//! **二、成批的活按区段摊，不在 `xFilter` 里一次做完。** 300ms 预算靠
//! `progress_handler` 实现，而它只在虚拟机指令之间触发：**一次长跑的
//! `xFilter` 是打断不了的**。所以 filter 只圈定区段，逐格推进放在 `xNext`。

use std::borrow::Cow;
use std::ffi::{c_int, CStr};
use std::sync::Arc;

use rusqlite::vtab::{
    Context, Filters, IndexConstraintOp, IndexFlags, IndexInfo, Module, VTab, VTabConfig,
    VTabConnection, VTabCursor,
};
use rusqlite::{ffi, Connection, Result};
use world::BlockMemory;

pub(crate) const BLOCKS_TABLE: &CStr = c"seen_blocks";
pub(crate) const EMPTY_TABLE: &CStr = c"seen_empty";

/// 三个坐标轴。计划编码按轴分组，两张表共用。
const AXES: usize = 3;

/// 把两张表装上同一份记忆快照。
///
/// 传进来的是**快照**，不是活的记忆：虚表在查询执行期间反复回读，若直接读
/// 活数据，一条跨越 300ms 的查询会在同一条 SQL 里看到前后不一致的世界，而且
/// 得在整个查询窗口里持锁——那会把眼睛（每 250ms）和寻路器（每 tick 上千次
/// 读）一起按住。分片之后快照只是克隆一层指针，这个代价可以忽略。
pub(crate) fn load_modules(connection: &Connection, memory: Arc<BlockMemory>) -> Result<()> {
    const BLOCKS: Module<BlocksTab> = Module::eponymous_only_module();
    const EMPTY: Module<EmptyTab> = Module::eponymous_only_module();
    connection.create_module(BLOCKS_TABLE, &BLOCKS, Some(memory.clone()))?;
    connection.create_module(EMPTY_TABLE, &EMPTY, Some(memory))
}

/// 一次查询在坐标上的取值范围。三态两张表共用同一种圈法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    min: [i32; 3],
    max: [i32; 3],
    /// 约束自相矛盾（如 `x=5 AND x=6`，或等值取了 i32 装不下的数）：一行都没有。
    impossible: bool,
}

impl Bounds {
    fn everything() -> Self {
        Self {
            min: [i32::MIN; AXES],
            max: [i32::MAX; AXES],
            impossible: false,
        }
    }

    fn contains(&self, at: [i32; 3]) -> bool {
        !self.impossible
            && (0..AXES).all(|axis| at[axis] >= self.min[axis] && at[axis] <= self.max[axis])
    }

    /// 三维都收成一点了吗。是的话走点探，不走区段遍历。
    fn as_point(&self) -> Option<[i32; 3]> {
        (!self.impossible && self.min == self.max).then_some(self.min)
    }
}

/// 共用的 xBestIndex：在 x/y/z 上收约束，编成计划位，报代价。
///
/// `rows_hint` 是本表的规模，用来给全扫那一档报价——它必须大到让规划器宁可
/// 把这张表放在嵌套循环内层。
fn plan(info: &mut IndexInfo, rows_hint: i64) -> Result<bool> {
    let mut equal: [Option<usize>; AXES] = [None; AXES];
    let mut lower: [Option<usize>; AXES] = [None; AXES];
    let mut upper: [Option<usize>; AXES] = [None; AXES];
    for (index, constraint) in info.constraints().enumerate() {
        if !constraint.is_usable() {
            continue;
        }
        let axis = match constraint.column() {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => continue,
        };
        // GT/LT 一律当成闭区间收（宁可圈宽），复查交给 SQLite——所以下面不 omit。
        let slot = match constraint.operator() {
            IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ => &mut equal[axis],
            IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GE
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_GT => &mut lower[axis],
            IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LE
            | IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_LT => &mut upper[axis],
            _ => continue,
        };
        // 同一轴上的第二个同类约束不收：收了却只执行一个就是漏判。
        if slot.is_none() {
            *slot = Some(index);
        }
    }

    // 位序固定为 eq(x,y,z) → lo(x,y,z) → hi(x,y,z)，filter 按同一顺序解码。
    let slots = [
        equal[0], equal[1], equal[2], lower[0], lower[1], lower[2], upper[0], upper[1], upper[2],
    ];
    let mut mask = 0_u32;
    let mut argv = 1;
    for (bit, slot) in slots.into_iter().enumerate() {
        let Some(index) = slot else { continue };
        mask |= 1 << bit;
        let mut usage = info.constraint_usage(index);
        usage.set_argv_index(argv);
        // 等值我们严格执行，可以让 SQLite 省掉复查；范围收宽了，必须留给它复查。
        if bit < AXES {
            usage.set_omit(true);
        }
        argv += 1;
    }

    let equalities = (mask & 0b111).count_ones() as usize;
    let total = rows_hint.max(1);
    if equalities == AXES {
        info.set_estimated_cost(1.0);
        info.set_estimated_rows(1);
        info.set_idx_flags(IndexFlags::SQLITE_INDEX_SCAN_UNIQUE);
    } else if mask != 0 {
        // 有约束但收不成一点：按区段圈，报一个明显低于全扫的价。
        let narrowed = (total / 64).max(1);
        info.set_estimated_cost(narrowed as f64);
        info.set_estimated_rows(narrowed);
    } else {
        // 无约束 = 全扫。**必须报大**：报小了规划器会把这张表拎到嵌套循环外层。
        info.set_estimated_cost(total as f64);
        info.set_estimated_rows(total);
    }
    info.set_idx_num(mask as c_int);
    Ok(true)
}

/// 按 [`plan`] 的位序解码出坐标范围。
fn decode(idx_num: c_int, args: &Filters<'_>) -> Bounds {
    let mask = idx_num as u32;
    let mut bounds = Bounds::everything();
    let mut argv = 0;
    for bit in 0..(AXES * 3) {
        if mask & (1 << bit) == 0 {
            continue;
        }
        let raw: std::result::Result<i64, _> = args.get(argv);
        argv += 1;
        let axis = bit % AXES;
        let Ok(value) = raw else {
            // 拿不到整数（`x = 'abc'` 之类）：没有哪一格配得上，一行不给。
            bounds.impossible = true;
            continue;
        };
        match bit / AXES {
            0 => match i32::try_from(value) {
                // 等值我们 omit 掉了 SQLite 的复查，所以这里必须精确。
                // i32 装不下的坐标world 里根本不存在，直接判空。
                Ok(exact) => {
                    bounds.min[axis] = bounds.min[axis].max(exact);
                    bounds.max[axis] = bounds.max[axis].min(exact);
                }
                Err(_) => bounds.impossible = true,
            },
            // 范围收宽是安全的：SQLite 会复查。
            1 => bounds.min[axis] = bounds.min[axis].max(clamp(value)),
            _ => bounds.max[axis] = bounds.max[axis].min(clamp(value)),
        }
    }
    if (0..AXES).any(|axis| bounds.min[axis] > bounds.max[axis]) {
        bounds.impossible = true;
    }
    bounds
}

fn clamp(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// 区段游标：先圈区段，再在区段内逐格吐。
///
/// 一次只把**一个区段**的候选摊成数组（至多 4096 个坐标），不是把整张表一次
/// 抖出来——这样 `xFilter` 不做成批的活，`xNext` 之间随时可以被预算打断。
struct Walk {
    sections: Vec<[i32; 3]>,
    section_cursor: usize,
    within: Vec<[i32; 3]>,
    within_cursor: usize,
    rowid: i64,
}

impl Walk {
    fn empty() -> Self {
        Self {
            sections: Vec::new(),
            section_cursor: 0,
            within: Vec::new(),
            within_cursor: 0,
            rowid: 0,
        }
    }

    /// 只有一格候选（点探）。
    fn single(hit: Option<[i32; 3]>) -> Self {
        Self {
            within: hit.into_iter().collect(),
            ..Self::empty()
        }
    }

    fn eof(&self) -> bool {
        self.within_cursor >= self.within.len()
    }

    fn current(&self) -> [i32; 3] {
        self.within[self.within_cursor]
    }

    fn step(&mut self) {
        self.within_cursor += 1;
        self.rowid += 1;
    }

    /// 当前区段吐完了就换下一个，直到拿到非空的一段或区段用尽。
    fn refill(&mut self, mut fill: impl FnMut([i32; 3]) -> Vec<[i32; 3]>) {
        while self.within_cursor >= self.within.len() {
            let Some(section) = self.sections.get(self.section_cursor).copied() else {
                return;
            };
            self.section_cursor += 1;
            self.within = fill(section);
            self.within_cursor = 0;
        }
    }
}

// ---- seen_blocks ----

#[repr(C)]
struct BlocksTab {
    base: ffi::sqlite3_vtab,
    memory: Arc<BlockMemory>,
    rows_hint: i64,
}

// SAFETY：契约是「结构体 #[repr(C)]，且 sqlite3_vtab 基类排在第一个字段」——
// BlocksTab 两条都满足。本 impl 自身不含任何 unsafe 块。
#[allow(unsafe_code)]
unsafe impl<'vtab> VTab<'vtab> for BlocksTab {
    type Aux = Arc<BlockMemory>;
    type Cursor = BlocksCursor;

    fn connect(
        db: &mut VTabConnection,
        aux: Option<&Self::Aux>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        _args: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        let memory = aux.cloned().unwrap_or_default();
        let rows_hint = memory.len() as i64;
        db.config(VTabConfig::Innocuous)?;
        Ok((
            Cow::Borrowed(
                c"CREATE TABLE x(x INTEGER, y INTEGER, z INTEGER, name TEXT, label TEXT, props TEXT)",
            ),
            Self {
                base: ffi::sqlite3_vtab::default(),
                memory,
                rows_hint,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        plan(info, self.rows_hint)
    }

    fn open(&'vtab mut self) -> Result<Self::Cursor> {
        Ok(BlocksCursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            memory: self.memory.clone(),
            bounds: Bounds::everything(),
            walk: Walk::empty(),
        })
    }
}

#[repr(C)]
struct BlocksCursor {
    base: ffi::sqlite3_vtab_cursor,
    memory: Arc<BlockMemory>,
    bounds: Bounds,
    walk: Walk,
}

impl BlocksCursor {
    fn refill(&mut self) {
        let memory = self.memory.clone();
        let bounds = self.bounds;
        self.walk.refill(move |section| {
            let mut positions: Vec<[i32; 3]> = memory
                .facts_in_section(section)
                .map(|(at, _)| at)
                .filter(|at| bounds.contains(*at))
                .collect();
            // 哈希表的迭代序不稳定；同一条查询两次跑出不同顺序会让模型无所适从。
            positions.sort_unstable();
            positions
        });
    }
}

// SAFETY：同上，sqlite3_vtab_cursor 基类在 BlocksCursor 的第一个字段。
#[allow(unsafe_code)]
unsafe impl VTabCursor for BlocksCursor {
    fn filter(&mut self, idx_num: c_int, _idx_str: Option<&str>, args: &Filters<'_>) -> Result<()> {
        self.bounds = decode(idx_num, args);
        if self.bounds.impossible {
            self.walk = Walk::empty();
            return Ok(());
        }
        // 点探：直接问那一格，不遍历区段。JOIN 里最热的就是这条路。
        if let Some(at) = self.bounds.as_point() {
            self.walk = Walk::single(self.memory.get(at).map(|_| at));
            return Ok(());
        }
        self.walk = Walk {
            sections: self
                .memory
                .section_keys_in(self.bounds.min, self.bounds.max),
            ..Walk::empty()
        };
        self.refill();
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.walk.step();
        self.refill();
        Ok(())
    }

    fn eof(&self) -> bool {
        self.walk.eof()
    }

    fn column(&self, context: &mut Context, index: c_int) -> Result<()> {
        let at = self.walk.current();
        match index {
            0 => context.set_result(&at[0]),
            1 => context.set_result(&at[1]),
            2 => context.set_result(&at[2]),
            _ => {
                // 负载只在查询真的要这一列时才取——物化那版无论查不查都全算一遍。
                let Some(fact) = self.memory.get(at) else {
                    return context.set_result(&rusqlite::types::Null);
                };
                match index {
                    3 => context.set_result(&fact.name),
                    4 => context
                        .set_result(&world::visible_block_label(&fact.name, &fact.properties)),
                    _ => context.set_result(
                        &serde_json::to_string(&fact.properties)
                            .unwrap_or_else(|_| "{}".to_owned()),
                    ),
                }
            }
        }
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.walk.rowid)
    }
}

// ---- seen_empty ----

#[repr(C)]
struct EmptyTab {
    base: ffi::sqlite3_vtab,
    memory: Arc<BlockMemory>,
    rows_hint: i64,
}

// SAFETY：同 BlocksTab——#[repr(C)] 且基类在首位。
#[allow(unsafe_code)]
unsafe impl<'vtab> VTab<'vtab> for EmptyTab {
    type Aux = Arc<BlockMemory>;
    type Cursor = EmptyCursor;

    fn connect(
        db: &mut VTabConnection,
        aux: Option<&Self::Aux>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        _args: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        let memory = aux.cloned().unwrap_or_default();
        let rows_hint = memory.known_empty_len() as i64;
        db.config(VTabConfig::Innocuous)?;
        Ok((
            Cow::Borrowed(c"CREATE TABLE x(x INTEGER, y INTEGER, z INTEGER)"),
            Self {
                base: ffi::sqlite3_vtab::default(),
                memory,
                rows_hint,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        plan(info, self.rows_hint)
    }

    fn open(&'vtab mut self) -> Result<Self::Cursor> {
        Ok(EmptyCursor {
            base: ffi::sqlite3_vtab_cursor::default(),
            memory: self.memory.clone(),
            bounds: Bounds::everything(),
            walk: Walk::empty(),
        })
    }
}

#[repr(C)]
struct EmptyCursor {
    base: ffi::sqlite3_vtab_cursor,
    memory: Arc<BlockMemory>,
    bounds: Bounds,
    walk: Walk,
}

impl EmptyCursor {
    fn refill(&mut self) {
        let memory = self.memory.clone();
        let bounds = self.bounds;
        self.walk.refill(move |section| {
            let mut positions: Vec<[i32; 3]> = memory
                .known_empty_in_section(section)
                .filter(|at| bounds.contains(*at))
                .collect();
            positions.sort_unstable();
            positions
        });
    }
}

// SAFETY：同 BlocksCursor——#[repr(C)] 且游标基类在首位。
#[allow(unsafe_code)]
unsafe impl VTabCursor for EmptyCursor {
    fn filter(&mut self, idx_num: c_int, _idx_str: Option<&str>, args: &Filters<'_>) -> Result<()> {
        self.bounds = decode(idx_num, args);
        if self.bounds.impossible {
            self.walk = Walk::empty();
            return Ok(());
        }
        if let Some(at) = self.bounds.as_point() {
            self.walk = Walk::single(self.memory.is_known_empty(at).then_some(at));
            return Ok(());
        }
        self.walk = Walk {
            sections: self
                .memory
                .section_keys_in(self.bounds.min, self.bounds.max),
            ..Walk::empty()
        };
        self.refill();
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.walk.step();
        self.refill();
        Ok(())
    }

    fn eof(&self) -> bool {
        self.walk.eof()
    }

    fn column(&self, context: &mut Context, index: c_int) -> Result<()> {
        let at = self.walk.current();
        context.set_result(&at[index.clamp(0, 2) as usize])
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.walk.rowid)
    }
}

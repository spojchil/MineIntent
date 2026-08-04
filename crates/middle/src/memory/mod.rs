//! Issue #127 的单文本记忆存储。
//!
//! 记忆不是旧版 `memories.json` 的结构化检索数据库：每次组装上下文时都从
//! `memory.md` 读取完整文本，写入只通过追加、锚定唯一替换或整文重写三种
//! 明确操作完成。写入前保留滚动备份，并在首次发现旧 JSON 文件时做一次
//! 确定性迁移。文件 I/O 放在 Tokio blocking pool；进程内路径锁串行化指向
//! 同一文件的所有 store 的 read-modify-write，不跨越用户回调或其他运行时锁。

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;
/// 默认单文本记忆文件名。
pub const DEFAULT_MEMORY_FILE: &str = "memory.md";
/// 旧结构化记忆文件名，仅用于一次性迁移。
pub const LEGACY_MEMORY_FILE: &str = "memories.json";

/// 一次写入前保留的主备份文件后缀。
pub const BACKUP_SUFFIX: &str = ".bak";

/// 单文本记忆操作。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryEdit {
    /// 在当前文本末尾追加文本。
    Append { text: String },
    /// 将唯一出现的旧文本替换为新文本；新文本为空表示删除。
    Replace { old_text: String, new_text: String },
    /// 用完整文本替换当前文件；空文本是合法的清空操作。
    Rewrite { text: String },
}

/// 结构化记忆存储错误。
#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory I/O failed during {operation} at {path}: {summary}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        summary: String,
    },
    #[error("memory worker failed: {summary}")]
    Worker { summary: String },
    #[error("old_text must not be empty")]
    EmptyAnchor,
    #[error("old_text must occur exactly once, found {count}")]
    AnchorNotUnique { count: usize },
    #[error("legacy memories.json is invalid: {summary}")]
    LegacyInvalid { summary: String },
}

impl MemoryError {
    /// 稳定机器可读错误码，供 Agent/tool 层包装而不解析 Display 文本。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "memory_io_failed",
            Self::Worker { .. } => "memory_worker_failed",
            Self::EmptyAnchor => "memory_empty_anchor",
            Self::AnchorNotUnique { .. } => "memory_anchor_not_unique",
            Self::LegacyInvalid { .. } => "memory_legacy_invalid",
        }
    }
}

/// 以单个文件为边界的可克隆 memory store。
#[derive(Clone)]
pub struct MemoryStore {
    path: Arc<PathBuf>,
    legacy_path: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryStore")
            .field("path", &self.path)
            .field("legacy_path", &self.legacy_path)
            .finish_non_exhaustive()
    }
}

impl MemoryStore {
    /// 使用给定单文本文件，并把同目录的 `memories.json` 作为迁移来源。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let legacy_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(LEGACY_MEMORY_FILE);
        Self::with_legacy(path, legacy_path)
    }

    /// 使用显式新旧路径，便于启动时接入既有数据目录。
    pub fn with_legacy(path: impl Into<PathBuf>, legacy_path: impl Into<PathBuf>) -> Self {
        let path = normalize_path(path.into());
        let legacy_path = normalize_path(legacy_path.into());
        Self {
            lock: path_lock(&path),
            path: Arc::new(path),
            legacy_path: Arc::new(legacy_path),
        }
    }

    /// 新文本文件的路径。
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// 每次读取都从磁盘获得当前全文；外部编辑会在下一次调用生效。
    pub async fn read_full(&self) -> Result<String, MemoryError> {
        let _guard = self.lock.lock().await;
        let path = Arc::clone(&self.path);
        let legacy = Arc::clone(&self.legacy_path);
        run_blocking(move || {
            migrate_if_needed(path.as_path(), legacy.as_path())?;
            read_text(path.as_path())
        })
        .await
    }

    /// 执行一个串行化的单文本编辑。
    pub async fn edit(&self, edit: MemoryEdit) -> Result<String, MemoryError> {
        let _guard = self.lock.lock().await;
        let path = Arc::clone(&self.path);
        let legacy = Arc::clone(&self.legacy_path);
        run_blocking(move || {
            migrate_if_needed(path.as_path(), legacy.as_path())?;
            let current = read_text(path.as_path())?;
            let force_write = matches!(&edit, MemoryEdit::Rewrite { .. });
            let next = apply_edit(&current, edit)?;
            if force_write || next != current {
                write_atomically(path.as_path(), &next)?;
            }
            Ok(next)
        })
        .await
    }

    /// 在文件末尾追加文本。
    pub async fn append(&self, text: impl Into<String>) -> Result<String, MemoryError> {
        self.edit(MemoryEdit::Append { text: text.into() }).await
    }

    /// 替换唯一锚点；不存在或重复时不写文件。
    pub async fn replace(
        &self,
        old_text: impl Into<String>,
        new_text: impl Into<String>,
    ) -> Result<String, MemoryError> {
        self.edit(MemoryEdit::Replace {
            old_text: old_text.into(),
            new_text: new_text.into(),
        })
        .await
    }

    /// 用完整文本重写文件。
    pub async fn rewrite(&self, text: impl Into<String>) -> Result<String, MemoryError> {
        self.edit(MemoryEdit::Rewrite { text: text.into() }).await
    }
}

async fn run_blocking<T, F>(operation: F) -> Result<T, MemoryError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, MemoryError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| MemoryError::Worker {
            summary: error.to_string(),
        })?
}

fn apply_edit(current: &str, edit: MemoryEdit) -> Result<String, MemoryError> {
    match edit {
        MemoryEdit::Append { text } => {
            if text.is_empty() {
                return Ok(current.to_owned());
            }
            Ok(format!("{current}{text}"))
        }
        MemoryEdit::Replace { old_text, new_text } => {
            if old_text.is_empty() {
                return Err(MemoryError::EmptyAnchor);
            }
            let count = occurrence_count(current, &old_text);
            if count != 1 {
                return Err(MemoryError::AnchorNotUnique { count });
            }
            Ok(current.replacen(&old_text, &new_text, 1))
        }
        MemoryEdit::Rewrite { text } => Ok(text),
    }
}

fn occurrence_count(text: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut offset = 0;
    while let Some(relative) = text[offset..].find(needle) {
        count += 1;
        let match_start = offset + relative;
        let Some(character) = text[match_start..].chars().next() else {
            break;
        };
        offset = match_start + character.len_utf8();
    }
    count
}

fn read_text(path: &Path) -> Result<String, MemoryError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(io_error("read", path, error)),
    }
}

fn migrate_if_needed(path: &Path, legacy_path: &Path) -> Result<(), MemoryError> {
    if path.exists() || !legacy_path.exists() {
        return Ok(());
    }

    let source = fs::read_to_string(legacy_path)
        .map_err(|error| io_error("read_legacy", legacy_path, error))?;
    let records = parse_legacy(&source)?;
    backup_existing(legacy_path)?;
    write_atomically(path, &render_legacy(records))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacyFile {
    protocol: String,
    records: Vec<LegacyRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LegacyRecord {
    protocol: String,
    id: String,
    world_id: String,
    kind: LegacyMemoryKind,
    summary: String,
    keywords: Vec<String>,
    evidence: Vec<LegacyEvidence>,
    created_at: String,
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyMemoryKind {
    Episode,
    Place,
    Commitment,
    PlayerPreference,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
struct LegacyEvidence {
    kind: LegacyEvidenceKind,
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyEvidenceKind {
    Event,
    ActionResult,
}

fn parse_legacy(source: &str) -> Result<Vec<LegacyRecord>, MemoryError> {
    let file: LegacyFile =
        serde_json::from_str(source).map_err(|error| MemoryError::LegacyInvalid {
            summary: error.to_string(),
        })?;
    if file.protocol != "mineintent.memory-file.v1" {
        return Err(MemoryError::LegacyInvalid {
            summary: format!("unsupported protocol {}", file.protocol),
        });
    }
    for record in &file.records {
        validate_legacy_record(record)?;
    }
    Ok(file.records)
}

fn render_legacy(mut records: Vec<LegacyRecord>) -> String {
    records.sort_by(|left, right| {
        compare_zod_datetimes(&left.created_at, &right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    records
        .into_iter()
        .map(|record| format!("{} ({})", record.summary, record.created_at))
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_legacy_record(record: &LegacyRecord) -> Result<(), MemoryError> {
    if record.protocol != "mineintent.memory.v1"
        || !is_zod_uuid(&record.id)
        || record.world_id.is_empty()
        || !matches!(
            &record.kind,
            LegacyMemoryKind::Episode
                | LegacyMemoryKind::Place
                | LegacyMemoryKind::Commitment
                | LegacyMemoryKind::PlayerPreference
        )
        || record.summary.is_empty()
        || utf16_len(&record.summary) > 1_000
        || record.keywords.len() > 32
        || record
            .keywords
            .iter()
            .any(|keyword| keyword.is_empty() || utf16_len(keyword) > 64)
        || record.evidence.is_empty()
        || record.evidence.len() > 64
        || record.evidence.iter().any(|evidence| {
            evidence.id.is_empty()
                || !matches!(
                    &evidence.kind,
                    LegacyEvidenceKind::Event | LegacyEvidenceKind::ActionResult
                )
        })
        || parse_zod_datetime(&record.created_at).is_none()
        || record.status != "active"
    {
        return Err(MemoryError::LegacyInvalid {
            summary: "record does not match mineintent.memory.v1".to_owned(),
        });
    }
    Ok(())
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn is_zod_uuid(value: &str) -> bool {
    if value == "00000000-0000-0000-0000-000000000000"
        || value == "ffffffff-ffff-ffff-ffff-ffffffffffff"
    {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
        && matches!(bytes[14], b'1'..=b'8')
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'A' | b'b' | b'B')
}

#[derive(Clone, Copy)]
struct ZodDateTime<'a> {
    date_and_time: (u16, u8, u8, u8, u8, u8),
    fraction: &'a [u8],
}

fn parse_zod_datetime(value: &str) -> Option<ZodDateTime<'_>> {
    let core = value.strip_suffix('Z')?;
    let (date, time) = core.split_once('T')?;
    if date.len() != 10 || date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' {
        return None;
    }
    let year = parse_digits::<u16>(&date.as_bytes()[0..4])?;
    let month = parse_digits::<u8>(&date.as_bytes()[5..7])?;
    let day = parse_digits::<u8>(&date.as_bytes()[8..10])?;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > days_in_month {
        return None;
    }

    let bytes = time.as_bytes();
    if bytes.len() < 5 || bytes[2] != b':' {
        return None;
    }
    let hour = parse_digits::<u8>(&bytes[0..2])?;
    let minute = parse_digits::<u8>(&bytes[3..5])?;
    if hour > 23 || minute > 59 {
        return None;
    }
    let (second, fraction) = match bytes.len() {
        5 => (0, &bytes[5..5]),
        8 if bytes[5] == b':' => (parse_digits::<u8>(&bytes[6..8])?, &bytes[8..8]),
        length if length > 9 && bytes[5] == b':' && bytes[8] == b'.' => {
            let fraction = &bytes[9..];
            if !fraction.iter().all(u8::is_ascii_digit) {
                return None;
            }
            (parse_digits::<u8>(&bytes[6..8])?, fraction)
        }
        _ => return None,
    };
    if second > 59 {
        return None;
    }
    Some(ZodDateTime {
        date_and_time: (year, month, day, hour, minute, second),
        fraction,
    })
}

fn parse_digits<T>(bytes: &[u8]) -> Option<T>
where
    T: TryFrom<u32>,
{
    bytes
        .iter()
        .try_fold(0_u32, |value, byte| {
            byte.is_ascii_digit()
                .then_some(value * 10 + u32::from(byte - b'0'))
        })?
        .try_into()
        .ok()
}

fn compare_zod_datetimes(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_zod_datetime(left), parse_zod_datetime(right)) {
        (Some(left), Some(right)) => left
            .date_and_time
            .cmp(&right.date_and_time)
            .then_with(|| compare_decimal_fractions(left.fraction, right.fraction)),
        _ => left.cmp(right),
    }
}

fn compare_decimal_fractions(left: &[u8], right: &[u8]) -> std::cmp::Ordering {
    (0..left.len().max(right.len()))
        .map(|index| {
            left.get(index)
                .copied()
                .unwrap_or(b'0')
                .cmp(&right.get(index).copied().unwrap_or(b'0'))
        })
        .find(|ordering| !ordering.is_eq())
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn write_atomically(path: &Path, contents: &str) -> Result<(), MemoryError> {
    if path.exists() {
        backup_existing(path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_error("create_parent", parent, error))?;
    }

    let temp = temporary_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| io_error("create_temp", &temp, error))?;
        set_private_mode(&file).map_err(|error| io_error("chmod_temp", &temp, error))?;
        file.write_all(contents.as_bytes())
            .map_err(|error| io_error("write_temp", &temp, error))?;
        file.flush()
            .map_err(|error| io_error("flush_temp", &temp, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync_temp", &temp, error))?;
        drop(file);

        replace_atomically(&temp, path).map_err(|error| io_error("replace_target", path, error))?;
        sync_parent_directory(path).map_err(|error| io_error("sync_parent", path, error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn backup_existing(path: &Path) -> Result<(), MemoryError> {
    if !path.exists() {
        return Ok(());
    }
    let backup = backup_path(path);
    let previous = previous_backup_path(path);
    if backup.exists() {
        if previous.exists() {
            fs::remove_file(&previous)
                .map_err(|error| io_error("remove_previous_backup", &previous, error))?;
        }
        fs::rename(&backup, &previous)
            .map_err(|error| io_error("rotate_backup", &backup, error))?;
    }
    fs::copy(path, &backup).map_err(|error| io_error("copy_backup", &backup, error))?;
    let file = OpenOptions::new()
        .write(true)
        .open(&backup)
        .map_err(|error| io_error("open_backup", &backup, error))?;
    set_private_mode(&file).map_err(|error| io_error("chmod_backup", &backup, error))?;
    file.sync_all()
        .map_err(|error| io_error("sync_backup", &backup, error))?;
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    append_path_suffix(path, BACKUP_SUFFIX)
}

fn previous_backup_path(path: &Path) -> PathBuf {
    append_path_suffix(path, &format!("{BACKUP_SUFFIX}.1"))
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    append_path_suffix(path, &format!(".tmp-{}-{nonce}", std::process::id()))
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

fn set_private_mode(_file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        _file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn io_error(operation: &'static str, path: &Path, error: io::Error) -> MemoryError {
    MemoryError::Io {
        operation,
        path: path.to_owned(),
        summary: error.to_string(),
    }
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let Some(file_name) = absolute.file_name() else {
        return absolute;
    };
    let Some(parent) = absolute.parent() else {
        return absolute;
    };
    fs::canonicalize(parent)
        .map(|canonical_parent| canonical_parent.join(file_name))
        .unwrap_or(absolute)
}

static PATH_LOCKS: OnceLock<std::sync::Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();

fn path_lock(path: &Path) -> Arc<Mutex<()>> {
    let registry = PATH_LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut registry = match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    registry.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = registry.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    registry.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}

#[cfg(unix)]
fn replace_atomically(temp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn replace_atomically(temp: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    // 全仓库唯一的 unsafe：Windows 没有安全封装的原子替换。标准库的
    // fs::rename 在目标存在时行为不保证原子，而记忆落盘必须要么是旧全文、
    // 要么是新全文，不能出现半截文件。指针仅指向本函数内构造、以 NUL 结尾
    // 的宽字符缓冲，生命周期覆盖整个调用。
    #[allow(unsafe_code)]
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let temp_wide = wide(temp);
    let target_wide = wide(target);
    #[allow(unsafe_code)]
    let success = unsafe {
        MoveFileExW(
            temp_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    // ReplaceFileW/MoveFileExW use WRITE_THROUGH; Windows has no portable directory fsync.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn replace_atomically(temp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(not(any(unix, windows)))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

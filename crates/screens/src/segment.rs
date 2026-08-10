//! 按原版聊天输入的机械上限切分文本。
//!
//! 一行等于一次输入循环。上限 256 以 UTF-16 单元计（Java 字符串长度的度量）。
//! 普通行超长时按字符边界切块继续发；命令行超长不可切（切开就不是那条命令），
//! 整次调用拒绝。

/// 原版聊天输入框的单条上限（UTF-16 单元）。
pub(crate) const MAX_CHAT_UTF16: usize = 256;

/// 把待发送文本规划成逐条发送的行序列。空白行丢弃。
/// 命令行（以 `/` 开头）超长时返回给模型的失败说明。
pub(crate) fn plan_lines(text: &str) -> Result<Vec<String>, String> {
    let mut planned = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let units = utf16_len(line);
        if line.starts_with('/') {
            if units > MAX_CHAT_UTF16 {
                return Err(format!(
                    "命令超过单条 {MAX_CHAT_UTF16} 字符上限（{units}），切开会破坏命令本身；请缩短后重试"
                ));
            }
            planned.push(line.to_owned());
        } else if units <= MAX_CHAT_UTF16 {
            planned.push(line.to_owned());
        } else {
            planned.extend(chunk_by_utf16(line, MAX_CHAT_UTF16));
        }
    }
    if planned.is_empty() {
        return Err("没有可发送的内容".to_owned());
    }
    Ok(planned)
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

fn chunk_by_utf16(line: &str, max_units: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut units = 0;
    for ch in line.chars() {
        let width = ch.len_utf16();
        if units + width > max_units {
            chunks.push(std::mem::take(&mut current));
            units = 0;
        }
        current.push(ch);
        units += width;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_and_blank_lines_drop() {
        let planned = plan_lines("你好\n\n  \n世界").unwrap();
        assert_eq!(planned, vec!["你好", "世界"]);
    }

    #[test]
    fn long_plain_line_chunks_at_utf16_boundary() {
        let long = "汉".repeat(300);
        let planned = plan_lines(&long).unwrap();
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].chars().count(), 256);
        assert_eq!(planned[1].chars().count(), 44);
    }

    #[test]
    fn command_line_passes_through_but_oversize_command_is_rejected() {
        let planned = plan_lines("/help").unwrap();
        assert_eq!(planned, vec!["/help"]);

        let oversize = format!("/tellraw {}", "x".repeat(300));
        let error = plan_lines(&oversize).unwrap_err();
        assert!(error.contains("命令超过"));
    }

    #[test]
    fn whitespace_only_input_is_rejected() {
        assert_eq!(plan_lines("  \n\t\n").unwrap_err(), "没有可发送的内容");
    }

    #[test]
    fn surrogate_pair_characters_count_as_two_units() {
        // 每个 emoji 占 2 个 UTF-16 单元：129 个 = 258 单元，必须切成两条。
        let line = "🌲".repeat(129);
        let planned = plan_lines(&line).unwrap();
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].chars().count(), 128);
        assert_eq!(planned[1].chars().count(), 1);
    }
}

use thiserror::Error;

pub const DEFAULT_MAX_SEGMENT_LENGTH: usize = 256;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SegmentChatError {
    #[error("max segment length must be positive")]
    InvalidMaxLength,
    #[error("speech text becomes empty after normalization")]
    EmptyAfterNormalization,
}

/// Normalizes and segments chat by Unicode code points (`char`), not bytes or grapheme clusters.
pub fn segment_chat(text: &str, max_length: usize) -> Result<Vec<String>, SegmentChatError> {
    if max_length == 0 {
        return Err(SegmentChatError::InvalidMaxLength);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut previous_was_space = true;
    for character in text.chars() {
        let is_space = character == '\0' || is_javascript_whitespace(character);
        if is_space {
            if !previous_was_space {
                normalized.push(' ');
            }
        } else {
            normalized.push(character);
        }
        previous_was_space = is_space;
    }
    let normalized = normalized.trim_matches(' ');
    if normalized.is_empty() {
        return Err(SegmentChatError::EmptyAfterNormalization);
    }

    let mut result = Vec::new();
    let mut rest = normalized.to_owned();
    while rest.chars().count() > max_length {
        let points: Vec<char> = rest.chars().collect();
        let lower_bound = (max_length.saturating_mul(3) / 5).max(1);
        let cut = (lower_bound..=max_length)
            .rev()
            .find(|index| is_segment_boundary(points[index - 1]))
            .unwrap_or(max_length);
        result.push(
            points[..cut]
                .iter()
                .collect::<String>()
                .trim_matches(' ')
                .to_owned(),
        );
        rest = points[cut..]
            .iter()
            .collect::<String>()
            .trim_matches(' ')
            .to_owned();
    }
    if !rest.is_empty() {
        result.push(rest);
    }
    Ok(result)
}

fn is_segment_boundary(character: char) -> bool {
    is_javascript_whitespace(character)
        || matches!(
            character,
            '，' | '。' | '！' | '？' | '、' | ',' | '.' | '!' | '?' | ';' | '；' | ':' | '：'
        )
}

pub(crate) fn is_javascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

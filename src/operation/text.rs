//! Text of a server reply as one printable line.

/// A character no printed field keeps: a control character or a Unicode format character (`Cf`),
/// such as a bidi override or a zero-width mark, that could reorder or hide the text of a line.
pub(crate) fn is_dropped(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{AD}'
                | '\u{600}'..='\u{605}'
                | '\u{61C}'
                | '\u{6DD}'
                | '\u{70F}'
                | '\u{890}'..='\u{891}'
                | '\u{8E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

/// `text` as one line: every dropped character (see [`is_dropped`]) removed and every run of
/// whitespace one space, so a field stays a field and a line stays a line.
pub(crate) fn squeeze(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_whitespace() || !is_dropped(*character))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

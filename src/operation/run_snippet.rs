//! The rules a `run SNIPPET` file passes before anything is sent: no control characters, lines
//! that fit an ABAP source line, every statement ended, and none of the statements that would open
//! or close a unit of the runner class around the snippet.
//!
//! The snippet is the user's own code, so the scan rules of the generated runner are not applied
//! to it; only the frame of the class keeps its scan. These rules keep the snippet inside its own
//! method: a statement that starts a method, class, form, or macro, or that includes another unit,
//! is refused. Statements are found with a tokenizer that knows comments, text literals, and
//! string templates, so several statements on one line and colon chains are judged one by one.

/// The widest line of an ABAP source file.
pub const MAX_LINE: usize = 255;

/// The first word of a statement that a snippet may not hold.
const REFUSED: [&str; 12] = [
    "METHOD",
    "ENDMETHOD",
    "METHODS",
    "CLASS",
    "ENDCLASS",
    "INTERFACE",
    "ENDINTERFACE",
    "FORM",
    "ENDFORM",
    "DEFINE",
    "END-OF-DEFINITION",
    "INCLUDE",
];

/// The lines of a snippet that passed the rules. An `Err` is the text of a `STOP`.
pub fn check(text: &str) -> Result<Vec<String>, String> {
    let lines: Vec<String> = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    let mut lines = lines;

    // A final line feed does not make an empty last line.
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    if lines.iter().all(|line| line.trim().is_empty()) {
        return Err("the snippet is empty".to_owned());
    }

    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;

        if line.chars().any(|c| c.is_control() || c == '\u{7f}') {
            return Err(format!(
                "snippet line {number} has a control character (a tab is one too)"
            ));
        }

        if line.chars().count() > MAX_LINE {
            return Err(format!(
                "snippet line {number} is longer than {MAX_LINE} characters"
            ));
        }
    }

    check_statements(&lines)?;

    Ok(lines)
}

/// Walks the text once. `statement` collects the characters of the statement in progress, outside
/// comments and literals, only to know whether one is open and what its first word is.
fn check_statements(lines: &[String]) -> Result<(), String> {
    let mut first_word = String::new();
    let mut in_word = true;
    let mut open_line = 0;

    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let chars: Vec<char> = line.chars().collect();
        let mut at = 0;

        // A line that starts with an asterisk is a comment line.
        if chars.first() == Some(&'*') {
            continue;
        }

        while at < chars.len() {
            let c = chars[at];

            match c {
                '"' => break,
                '\'' | '`' => {
                    at = skip_literal(&chars, at, number)?;
                    mark_open(&mut open_line, number);
                    in_word = false;
                }
                '|' => {
                    at = skip_template(&chars, at, number)?;
                    mark_open(&mut open_line, number);
                    in_word = false;
                }
                '.' => {
                    refuse_statement(&first_word, number)?;
                    first_word.clear();
                    in_word = true;
                    open_line = 0;
                    at += 1;
                }
                ':' | ',' => {
                    // A colon ends the first word; the chain keeps the statement open.
                    in_word = false;
                    at += 1;
                }
                c if c.is_whitespace() => {
                    if !first_word.is_empty() {
                        in_word = false;
                    }

                    at += 1;
                }
                c => {
                    if in_word {
                        first_word.push(c.to_ascii_uppercase());
                    }

                    mark_open(&mut open_line, number);
                    at += 1;
                }
            }
        }
    }

    if open_line != 0 {
        return Err(format!(
            "the snippet ends inside a statement that starts on line {open_line}: a period is missing"
        ));
    }

    Ok(())
}

fn mark_open(open_line: &mut usize, number: usize) {
    if *open_line == 0 {
        *open_line = number;
    }
}

fn refuse_statement(word: &str, number: usize) -> Result<(), String> {
    if REFUSED.contains(&word) {
        return Err(format!(
            "snippet line {number}: {word} is not allowed in a snippet; the snippet is the body \
             of one method, so it cannot open or close a method, class, form, or macro, or \
             include another unit"
        ));
    }

    Ok(())
}

/// The index after a text literal that starts at `at`; a doubled quote stays inside it.
fn skip_literal(chars: &[char], at: usize, number: usize) -> Result<usize, String> {
    let quote = chars[at];
    let mut index = at + 1;

    while index < chars.len() {
        if chars[index] == quote {
            if chars.get(index + 1) == Some(&quote) {
                index += 2;

                continue;
            }

            return Ok(index + 1);
        }

        index += 1;
    }

    Err(format!(
        "snippet line {number}: a text literal is not closed"
    ))
}

/// The index after a string template that starts at `at`. A backslash escapes the next character;
/// an embedded expression in braces may hold literals and templates of its own.
fn skip_template(chars: &[char], at: usize, number: usize) -> Result<usize, String> {
    let mut index = at + 1;

    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            '|' => return Ok(index + 1),
            '{' => index = skip_expression(chars, index, number)?,
            _ => index += 1,
        }
    }

    Err(format!(
        "snippet line {number}: a string template is not closed"
    ))
}

/// The index after the closing brace of an embedded expression that starts at `at`.
fn skip_expression(chars: &[char], at: usize, number: usize) -> Result<usize, String> {
    let mut index = at + 1;

    while index < chars.len() {
        match chars[index] {
            '}' => return Ok(index + 1),
            '\'' | '`' => index = skip_literal(chars, index, number)?,
            '|' => index = skip_template(chars, index, number)?,
            _ => index += 1,
        }
    }

    Err(format!(
        "snippet line {number}: an embedded expression is not closed"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(text: &str) -> String {
        check(text).unwrap_err()
    }

    #[test]
    fn a_plain_snippet_passes_and_keeps_its_lines() {
        let lines = check("DATA(lv_x) = 1.\nprint( |x { lv_x }| ).\n").unwrap();

        assert_eq!(lines, ["DATA(lv_x) = 1.", "print( |x { lv_x }| )."]);
        assert_eq!(check("a = 1.\r\nb = 2.\r\n").unwrap(), ["a = 1.", "b = 2."]);
    }

    #[test]
    fn every_refused_statement_is_found_with_its_line() {
        for word in REFUSED {
            let text = format!("a = 1.\n{} x.\n", word.to_ascii_lowercase());
            let error = refused(&text);

            assert!(error.starts_with("snippet line 2: "), "{word}: {error}");
            assert!(error.contains(word), "{word}: {error}");
        }
    }

    #[test]
    fn several_statements_on_one_line_are_judged_one_by_one() {
        let error = refused("a = 1. ENDMETHOD. b = 2.");

        assert!(error.contains("ENDMETHOD"), "{error}");
        assert!(refused("a = 1. b = 2. include zfoo.").contains("INCLUDE"));
    }

    #[test]
    fn a_colon_chain_keeps_the_word_before_the_colon() {
        assert!(refused("INCLUDE: zfoo, zbar.").contains("INCLUDE"));
        assert!(refused("methods: a, b.").contains("METHODS"));
        // The elements of a chain are not statements of their own.
        check("DATA: a TYPE i, b TYPE i.").unwrap();
        check("TYPES: BEGIN OF s, a TYPE i, END OF s.").unwrap();
        check("CLASS-DATA: a TYPE i.").unwrap();
    }

    #[test]
    fn words_inside_comments_and_literals_do_not_count() {
        check("\" ENDMETHOD.\na = 1. \" CLASS x.\n* INCLUDE zfoo.\nb = 'ENDMETHOD.' && `CLASS.`.")
            .unwrap();
        check("a = |ENDMETHOD. { 'INCLUDE.' } done|.").unwrap();
        check("a = 'it''s. ENDMETHOD.'.").unwrap();
    }

    #[test]
    fn a_snippet_has_to_end_its_last_statement() {
        let error = refused("a = 1.\nb = 2");

        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("period"), "{error}");
        // A closing comment is not a statement.
        check("a = 1.\n\" done").unwrap();
    }

    #[test]
    fn an_open_literal_or_template_is_refused() {
        assert!(refused("a = 'abc.").contains("not closed"));
        assert!(refused("a = `abc.").contains("not closed"));
        assert!(refused("a = |abc.").contains("not closed"));
        assert!(refused("a = |abc { x.|").contains("not closed"));
    }

    #[test]
    fn control_characters_and_wide_lines_are_refused() {
        assert!(refused("a\t= 1.").contains("control character"));
        assert!(refused("a = 1.\u{7}").contains("line 1"));
        assert!(refused(&format!("a = '{}'.", "x".repeat(MAX_LINE))).contains("longer than"));
        check(&format!("a = '{}'.", "x".repeat(MAX_LINE - 7))).unwrap();
    }
}

//! The safety scanner of the runner class source.
//!
//! The class may call only standard SAP function modules and classes from the allow-lists below,
//! and no name starting with `Y` or `Z` may appear except the runner class itself. The scan reads
//! the whole text, comments and literals included, so a template author cannot hide a name there.
//! A payload line (see `runner_template::payload_literal`) is data and is skipped.

use super::runner_template::payload_literal;

/// Function modules the templates may call (facts U1 to U6 of M6).
pub const ALLOWED_FUNCTIONS: &[&str] = &[
    "DDIF_DOMA_ACTIVATE",
    "DDIF_DOMA_GET",
    "DDIF_DOMA_PUT",
    "DDIF_DTEL_ACTIVATE",
    "DDIF_DTEL_GET",
    "DDIF_DTEL_PUT",
    "DDIF_TABL_ACTIVATE",
    "DDIF_TABL_GET",
    "DDIF_TABL_PUT",
    "DDIF_TTYP_ACTIVATE",
    "DDIF_TTYP_GET",
    "DDIF_TTYP_PUT",
    "DD_DECIDE",
    "DD_GET_NAMETAB",
    "DD_NTAB_GEN",
    "DD_TABL_EXPAND",
    "FUNCTION_EXISTS",
    "RPY_TRANSACTION_DELETE",
    "RPY_TRANSACTION_INSERT",
    "RS_ACCESS_PERMISSION",
    "RS_CORR_INSERT",
    "RS_DD_DELETE_OBJ",
    "TR_SYS_PARAMS",
    "TR_TADIR_INTERFACE",
];

/// Classes and interfaces the templates may name. `CL_ABAP_CONV_OUT_CE` encodes SAP text as
/// UTF-8 for the output protocol, `CL_ABAP_MESSAGE_DIGEST` gives the SHA-256 of a live
/// definition (the plan hash of `TRAN`), `CL_ABAP_TYPEDESCR` and `CL_ABAP_STRUCTDESCR` list the
/// components of a DDIC structure so that a failed proof names the first difference, and
/// `CX_ROOT` catches an unexpected exception; all are proven live in Task 12A.
pub const ALLOWED_CLASSES: &[&str] = &[
    "CL_ABAP_CONV_IN_CE",
    "CL_ABAP_CONV_OUT_CE",
    "CL_ABAP_MESSAGE_DIGEST",
    "CL_ABAP_STRUCTDESCR",
    "CL_ABAP_TYPEDESCR",
    "CL_HTTP_UTILITY",
    "CX_ROOT",
    "IF_OO_ADT_CLASSRUN",
    "IF_OO_ADT_CLASSRUN_OUT",
    "IF_OO_ADT_INTRNL_CLASSRUN",
];

/// Function modules only the `RUN_PROG` template may call: the list the submitted report writes to
/// memory, and the selection screen of the report (interface verified live).
pub const RUN_PROG_FUNCTIONS: &[&str] = &[
    "LIST_FREE_MEMORY",
    "LIST_FROM_MEMORY",
    "LIST_TO_ASCI",
    "RS_REPORTSELECTIONS_INFO",
];

/// Classes only the `RUN_PROG` template may name: the capture of an ALV grid, and the exception of
/// the source hash.
pub const RUN_PROG_CLASSES: &[&str] = &[
    "CL_SALV_BS_RUNTIME_INFO",
    "CX_ABAP_MESSAGE_DIGEST",
    "CX_SALV_BS_SC_RUNTIME_INFO",
];

/// The two statements the `RUN_PROG` template may hold: the generated `SUBMIT`, with no variant
/// and with one. Every token of the statement is fixed, so no value reaches it except through the
/// variables it names. Every other mode refuses `SUBMIT` in any shape.
const SUBMIT_SHAPES: [&[&str]; 2] = [
    &[
        "submit",
        "(",
        "lv_report",
        ")",
        "with",
        "selection",
        "-",
        "table",
        "mt_sel",
        "exporting",
        "list",
        "to",
        "memory",
        "and",
        "return",
    ],
    &[
        "submit",
        "(",
        "lv_report",
        ")",
        "using",
        "selection",
        "-",
        "set",
        "lv_variant",
        "with",
        "selection",
        "-",
        "table",
        "mt_sel",
        "exporting",
        "list",
        "to",
        "memory",
        "and",
        "return",
    ],
];

/// What a scan allows beyond the allow-lists of every template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanMode {
    /// Every template except `RUN_PROG`.
    Strict,
    /// The `RUN_PROG` template: the generated `SUBMIT` and the names it needs.
    RunProg,
}

/// Generic reference types that name no class.
const GENERIC_TYPES: [&str; 2] = ["DATA", "OBJECT"];

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Text(String),
    Symbol(char),
    Arrow,
}

/// Reports every violation as `line N: ...`. `runner` is the one `Y`/`Z` name that may appear:
/// the class name, given when the whole source is scanned and absent for the constant part.
pub fn scan(code: &str, runner: Option<&str>) -> Result<(), Vec<String>> {
    scan_as(code, runner, ScanMode::Strict)
}

/// [`scan`] with the rules of `mode`.
pub fn scan_as(code: &str, runner: Option<&str>, mode: ScanMode) -> Result<(), Vec<String>> {
    let tokens = tokenize(code);
    let mut found = Vec::new();

    for (index, (token, line)) in tokens.iter().enumerate() {
        match token {
            Token::Word(word) => name_rule(word, *line, runner, &mut found),
            Token::Text(text) => text
                .split(|c: char| !is_word(c))
                .for_each(|word| name_rule(word, *line, runner, &mut found)),
            _ => {}
        }

        statement_rules(&tokens, index, mode, &mut found);
        danger_rules(&tokens, index, mode, &mut found);
    }

    logic_rule(&tokens, &mut found);

    if found.is_empty() { Ok(()) } else { Err(found) }
}

/// A table expression `name[ ... ]` raises when the row is missing, and the kernel does not stop
/// a condition at the first operand that decides it (release 753 ended a method in an internal
/// error). So no statement that holds AND or OR may hold one, except as the operand of
/// `line_exists` or `line_index`, which do not raise. Guard it with a nested IF instead.
fn logic_rule(tokens: &[(Token, usize)], found: &mut Vec<String>) {
    for statement in tokens.split(|(token, _)| matches!(token, Token::Symbol('.'))) {
        let logic = statement.iter().any(|(token, _)| {
            matches!(token, Token::Word(word)
                if word.eq_ignore_ascii_case("AND") || word.eq_ignore_ascii_case("OR"))
        });

        if !logic {
            continue;
        }

        for (index, (token, line)) in statement.iter().enumerate() {
            let table_expression = matches!(token, Token::Symbol('['))
                && index > 0
                && matches!(statement[index - 1].0, Token::Word(_));
            let safe = index >= 3
                && matches!(&statement[index - 3].0, Token::Word(word)
                    if word.eq_ignore_ascii_case("line_exists")
                        || word.eq_ignore_ascii_case("line_index"))
                && matches!(statement[index - 2].0, Token::Symbol('('));

            if table_expression && !safe {
                found.push(format!(
                    "line {line}: a table expression in a condition with AND or OR can raise; use a nested IF"
                ));
            }
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A word of two or more characters that starts with Y or Z is a customer name. One letter is
/// the range test of the name guard.
fn name_rule(word: &str, line: usize, runner: Option<&str>, found: &mut Vec<String>) {
    let customer = word.len() > 1 && word.starts_with(['Y', 'Z', 'y', 'z']);

    if customer && !runner.is_some_and(|name| name.eq_ignore_ascii_case(word)) {
        found.push(format!("line {line}: Y/Z name {word} is not allowed"));
    }
}

fn tokenize(code: &str) -> Vec<(Token, usize)> {
    let mut tokens = Vec::new();

    for (index, text) in code.lines().enumerate() {
        let line = index + 1;

        if payload_literal(text).is_some() {
            continue;
        }

        let chars: Vec<char> = text.chars().collect();
        let mut at = 0;

        while at < chars.len() {
            let c = chars[at];

            if is_word(c) {
                let end = chars[at..].iter().take_while(|c| is_word(**c)).count() + at;

                tokens.push((Token::Word(chars[at..end].iter().collect()), line));
                at = end;
            } else if c == '\'' || c == '`' {
                let end = chars[at + 1..]
                    .iter()
                    .position(|other| *other == c)
                    .map_or(chars.len(), |close| at + 1 + close);

                tokens.push((Token::Text(chars[at + 1..end].iter().collect()), line));
                at = end + 1;
            } else if c == '=' && chars.get(at + 1) == Some(&'>') {
                tokens.push((Token::Arrow, line));
                at += 2;
            } else {
                if !c.is_whitespace() {
                    tokens.push((Token::Symbol(c), line));
                }
                at += 1;
            }
        }
    }

    tokens
}

fn word_at(tokens: &[(Token, usize)], index: usize, expected: &str) -> bool {
    matches!(tokens.get(index), Some((Token::Word(word), _)) if word.eq_ignore_ascii_case(expected))
}

fn next_word(tokens: &[(Token, usize)], index: usize) -> Option<&str> {
    match tokens.get(index) {
        Some((Token::Word(word), _)) => Some(word),
        _ => None,
    }
}

fn class_rule(name: &str, line: usize, generic: bool, mode: ScanMode, found: &mut Vec<String>) {
    let upper = name.to_ascii_uppercase();
    let allowed = ALLOWED_CLASSES.contains(&upper.as_str())
        || (mode == ScanMode::RunProg && RUN_PROG_CLASSES.contains(&upper.as_str()))
        || (generic && GENERIC_TYPES.contains(&upper.as_str()));

    if !allowed {
        found.push(format!(
            "line {line}: class {name} is not on the allow-list"
        ));
    }
}

/// The statement patterns that name a function module or a class.
fn statement_rules(
    tokens: &[(Token, usize)],
    index: usize,
    mode: ScanMode,
    found: &mut Vec<String>,
) {
    let line = tokens[index].1;

    match &tokens[index].0 {
        Token::Word(word) if word.eq_ignore_ascii_case("DESTINATION") => {
            found.push(format!("line {line}: DESTINATION is not allowed"));
        }
        Token::Word(word)
            if word.eq_ignore_ascii_case("CALL") && word_at(tokens, index + 1, "FUNCTION") =>
        {
            match tokens.get(index + 2) {
                Some((Token::Text(name), _))
                    if ALLOWED_FUNCTIONS.contains(&name.to_ascii_uppercase().as_str())
                        || (mode == ScanMode::RunProg
                            && RUN_PROG_FUNCTIONS
                                .contains(&name.to_ascii_uppercase().as_str())) => {}
                Some((Token::Text(name), _)) => found.push(format!(
                    "line {line}: CALL FUNCTION '{name}' is not on the allow-list"
                )),
                _ => found.push(format!(
                    "line {line}: CALL FUNCTION needs a literal function name"
                )),
            }
        }
        Token::Word(word) => {
            if matches!(tokens.get(index + 1), Some((Token::Arrow, _))) {
                class_rule(word, line, false, mode, found);
            }

            let upper = word.to_ascii_uppercase();
            let after = |offset| next_word(tokens, index + offset);

            match upper.as_str() {
                "REF" if word_at(tokens, index + 1, "TO") => {
                    after(2)
                        .iter()
                        .for_each(|name| class_rule(name, line, true, mode, found));
                }
                "NEW" | "CAST" | "INTERFACES" => {
                    after(1)
                        .iter()
                        .for_each(|name| class_rule(name, line, false, mode, found));
                }
                "INHERITING" if word_at(tokens, index + 1, "FROM") => {
                    after(2)
                        .iter()
                        .for_each(|name| class_rule(name, line, false, mode, found));
                }
                "EXCEPTION" if word_at(tokens, index + 1, "TYPE") => {
                    after(2)
                        .iter()
                        .for_each(|name| class_rule(name, line, false, mode, found));
                }
                "CATCH" => catch_rule(tokens, index + 1, mode, found),
                "CREATE" if word_at(tokens, index + 1, "OBJECT") => {
                    create_rule(tokens, index + 2, mode, found)
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Statements that run other code or change the database without naming a function module or a
/// class. No template uses one, so none is allowed. `MODIFY` and `DELETE dbtab FROM wa` read the
/// same as the internal-table forms, so every `MODIFY` and a `DELETE .. FROM` with a work area
/// are refused (shortcut: a template that needs one gets an explicit allow entry here).
fn danger_rules(tokens: &[(Token, usize)], index: usize, mode: ScanMode, found: &mut Vec<String>) {
    let line = tokens[index].1;
    let mut refuse = |what: &str| found.push(format!("line {line}: {what} is not allowed"));
    let symbol_at = |offset: usize, expected: char| {
        tokens.get(index + offset).map(|(token, _)| token) == Some(&Token::Symbol(expected))
    };
    let at = |offset: usize, expected: &str| word_at(tokens, index + offset, expected);

    match &tokens[index].0 {
        Token::Arrow if symbol_at(1, '(') => refuse("a dynamic method call"),
        Token::Symbol('-') if symbol_at(1, '>') && symbol_at(2, '(') => {
            refuse("a dynamic method call")
        }
        Token::Word(word) if starts(tokens, index) => {
            let index_from = next_word(tokens, index + 3)
                .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_digit()));

            match word.to_ascii_uppercase().as_str() {
                "SUBMIT" if mode == ScanMode::RunProg && is_generated_submit(tokens, index) => {}
                "SUBMIT" => refuse("SUBMIT"),
                "GENERATE" => refuse("GENERATE"),
                "EXEC" if at(1, "SQL") => refuse("EXEC SQL"),
                "CALL" if matches!(tokens.get(index + 1), Some((Token::Text(_), _))) => {
                    refuse("a kernel CALL")
                }
                "CALL" if at(1, "TRANSACTION") => refuse("CALL TRANSACTION"),
                "CALL" if at(1, "METHOD") && symbol_at(2, '(') => refuse("a dynamic method call"),
                "INSERT" if at(1, "REPORT") => refuse("INSERT REPORT"),
                "INSERT" if (at(1, "INTO") || at(2, "FROM") || symbol_at(1, '(')) => {
                    refuse("a database INSERT")
                }
                "UPDATE" => refuse("a database UPDATE"),
                "MODIFY" => refuse("MODIFY"),
                "DELETE"
                    if (at(1, "FROM") || symbol_at(1, '(') || (at(2, "FROM") && !index_from)) =>
                {
                    refuse("a database DELETE")
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// A statement starts after a period or a colon, or on a new line; a word inside a comment or
/// after another word does not.
fn starts(tokens: &[(Token, usize)], index: usize) -> bool {
    index == 0
        || matches!(tokens[index - 1], (Token::Symbol('.' | ':'), _))
        || tokens[index - 1].1 < tokens[index].1
}

/// `CATCH a b INTO x`: every word up to `INTO` or the end of the statement is a class.
fn catch_rule(tokens: &[(Token, usize)], from: usize, mode: ScanMode, found: &mut Vec<String>) {
    for (token, line) in &tokens[from.min(tokens.len())..] {
        match token {
            Token::Word(word) if word.eq_ignore_ascii_case("INTO") => break,
            Token::Word(word)
                if ["BEFORE", "UNWIND"].contains(&word.to_ascii_uppercase().as_str()) => {}
            Token::Word(word) => class_rule(word, *line, false, mode, found),
            _ => break,
        }
    }
}

/// `CREATE OBJECT x TYPE c`: the word after `TYPE` before the statement ends is a class.
fn create_rule(tokens: &[(Token, usize)], from: usize, mode: ScanMode, found: &mut Vec<String>) {
    for index in from.min(tokens.len())..tokens.len() {
        match &tokens[index].0 {
            Token::Symbol('.') => break,
            Token::Word(word) if word.eq_ignore_ascii_case("TYPE") => {
                if let Some(name) = next_word(tokens, index + 1) {
                    class_rule(name, tokens[index].1, false, mode, found);
                }
                break;
            }
            _ => {}
        }
    }
}

/// Whether the statement that starts at `index` is, token for token, one of the generated
/// `SUBMIT` shapes. A text literal, a comment, or any other word makes it a different statement.
fn is_generated_submit(tokens: &[(Token, usize)], index: usize) -> bool {
    let statement: Vec<String> = tokens[index..]
        .iter()
        .take_while(|(token, _)| !matches!(token, Token::Symbol('.')))
        .map(|(token, _)| match token {
            Token::Word(word) => word.to_ascii_lowercase(),
            Token::Symbol(symbol) => symbol.to_string(),
            Token::Text(_) | Token::Arrow => String::new(),
        })
        .collect();

    // The statement must also end with a period inside the scanned text.
    let ended = tokens[index..]
        .iter()
        .any(|(token, _)| matches!(token, Token::Symbol('.')));

    ended
        && SUBMIT_SHAPES.iter().any(|shape| {
            statement
                .iter()
                .map(String::as_str)
                .eq(shape.iter().copied())
        })
}

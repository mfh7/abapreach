//! The source generator of the temporary runner class.
//!
//! The class is constant code plus data. Constant code is the shared core (payload decode, value
//! re-validation, the output protocol, the system and client guards, and the helpers that every
//! type needs) and the part of one type template. The only variable token inside code is the
//! validated class name, which stands on exactly two lines: `CLASS <name> DEFINITION` and
//! `CLASS <name> IMPLEMENTATION.` The payload (see `payload.rs`) is base64 and goes into one
//! method as literal lines; no user value is ever spliced into code. `template_hash` pins the
//! constant part, and `runner_scan` checks it against the allow-list of standard SAP objects.

use sha2::{Digest, Sha256};

/// Stands in for the class name wherever the constant part is shown, hashed, or scanned.
pub const NAME_PLACEHOLDER: &str = "<NAME>";

const CORE_DECLARATIONS: &str = include_str!("runner_template/core_decl.abap");
const CORE_METHODS: &str = include_str!("runner_template/core_impl.abap");

/// A class name is at most 30 characters and starts with `Y` or `Z`.
const MAX_NAME: usize = 30;
/// The widest payload line the encoder writes.
const MAX_PAYLOAD_LITERAL: usize = 200;
const PAYLOAD_PREFIX: &str = "    lv_b64 = lv_b64 && '";
const PAYLOAD_SUFFIX: &str = "'.";

const PAYLOAD_HEAD: [&str; 4] = ["", "", "  METHOD payload.", "    DATA lv_b64 TYPE string."];
const PAYLOAD_TAIL: [&str; 3] = ["    rv_b64 = lv_b64.", "  ENDMETHOD.", "ENDCLASS."];

/// The part of the class that belongs to one object type. `declarations` are private-section
/// lines after the core's own; `methods` are method implementations and must include
/// `run_type`, which the core calls once the guards passed.
pub struct TypeTemplate {
    pub name: &'static str,
    pub declarations: &'static str,
    pub methods: &'static str,
}

/// The core with a `run_type` that refuses every request. It pins the shared part; the type
/// templates each add their own entry to `TEMPLATES`.
pub static CORE_ONLY: TypeTemplate = TypeTemplate {
    name: "CORE",
    declarations: "",
    methods: include_str!("runner_template/core_only.abap"),
};

/// The `TRAN` template: read, create, and delete of a report transaction.
pub static TRAN_TEMPLATE: TypeTemplate = TypeTemplate {
    name: "TRAN",
    declarations: include_str!("runner_template/tran_decl.abap"),
    methods: include_str!("runner_template/tran_impl.abap"),
};

/// The DDIC templates. The four types share one body (the payload names the type), and each has
/// its own entry so that a run selects its template by type and the pins stay per type.
pub static DTEL_TEMPLATE: TypeTemplate = ddic_template("DTEL");
pub static DOMA_TEMPLATE: TypeTemplate = ddic_template("DOMA");
pub static TABL_TEMPLATE: TypeTemplate = ddic_template("TABL");
pub static TTYP_TEMPLATE: TypeTemplate = ddic_template("TTYP");

const fn ddic_template(name: &'static str) -> TypeTemplate {
    TypeTemplate {
        name,
        declarations: include_str!("runner_template/ddic_decl.abap"),
        methods: include_str!("runner_template/ddic_impl.abap"),
    }
}

pub static TEMPLATES: [&TypeTemplate; 6] = [
    &CORE_ONLY,
    &TRAN_TEMPLATE,
    &DTEL_TEMPLATE,
    &DOMA_TEMPLATE,
    &TABL_TEMPLATE,
    &TTYP_TEMPLATE,
];

/// The constant lines of a template, with the placeholder where the class name goes and without
/// payload lines.
pub fn template_lines(template: &TypeTemplate) -> Vec<String> {
    assemble(template, NAME_PLACEHOLDER, &[])
}

/// SHA-256 over the constant lines. The name and the payload cannot reach it, so a change of the
/// hash means a change of code.
pub fn template_hash(template: &TypeTemplate) -> String {
    let digest = Sha256::digest(template_lines(template).join("\n").as_bytes());

    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The class source: the constant lines, the class name on its two lines, and the payload lines.
/// The name and every payload line are checked here, so nothing outside the safe set can reach
/// the source.
pub fn generate(template: &TypeTemplate, name: &str, payload: &[String]) -> Result<String, String> {
    check_name(name)?;

    if payload.is_empty() {
        return Err("the runner payload is empty".to_owned());
    }

    for line in payload {
        if payload_literal(&payload_line(line)).is_none() {
            return Err(format!(
                "a runner payload line must be 1 to {MAX_PAYLOAD_LITERAL} base64 characters: {line:?}"
            ));
        }
    }

    let mut source = assemble(template, name, payload).join("\n");

    source.push('\n');

    Ok(source)
}

/// The source line that carries one payload literal.
fn payload_line(literal: &str) -> String {
    format!("{PAYLOAD_PREFIX}{literal}{PAYLOAD_SUFFIX}")
}

/// The literal of a payload line, or nothing when the line is any other line. The scanner skips
/// exactly these lines.
pub fn payload_literal(line: &str) -> Option<&str> {
    let literal = line
        .strip_prefix(PAYLOAD_PREFIX)?
        .strip_suffix(PAYLOAD_SUFFIX)?;
    let base64 = literal
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='));

    (!literal.is_empty() && literal.len() <= MAX_PAYLOAD_LITERAL && base64).then_some(literal)
}

fn check_name(name: &str) -> Result<(), String> {
    let valid = name.starts_with(['Y', 'Z'])
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');

    if valid {
        Ok(())
    } else {
        Err(format!(
            "a runner class name starts with Y or Z, has at most {MAX_NAME} characters of A-Z 0-9 _: {name:?}"
        ))
    }
}

fn assemble(template: &TypeTemplate, name: &str, payload: &[String]) -> Vec<String> {
    let mut lines = vec![format!("CLASS {name} DEFINITION")];

    lines.extend(text_lines(CORE_DECLARATIONS));
    lines.extend(text_lines(template.declarations));
    lines.extend(["ENDCLASS.".to_owned(), String::new(), String::new()]);
    lines.push(format!("CLASS {name} IMPLEMENTATION."));
    lines.extend(text_lines(CORE_METHODS));
    lines.extend(text_lines(template.methods));
    lines.extend(PAYLOAD_HEAD.map(str::to_owned));
    lines.extend(payload.iter().map(|literal| payload_line(literal)));
    lines.extend(PAYLOAD_TAIL.map(str::to_owned));

    lines
}

/// Splits on line ends only, so a checkout with CRLF gives the same lines.
fn text_lines(text: &str) -> impl Iterator<Item = String> {
    text.lines().map(str::to_owned)
}

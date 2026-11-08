//! The local rules of the DDIC and `TRAN` commands: which options a type takes and the limits of
//! its names, decided before any connection is opened. Every `Err` is the text of a `STOP`.

use crate::cli::{DeleteKind, DeleteTarget, IncludeKind, LineRange, Subc};
use crate::hash::validate_name_within;
use crate::object::{check_package_scope, check_request};
use crate::operation::payload::Kind;
use std::path::Path;

/// The `--prog` rule, for the type that needs it and for every type that does not take it.
pub const PROG_RULE: &str = "create TRAN needs --prog <report>; --prog only applies to TRAN";
/// The text of a `TRAN` description is a CHAR 36 field.
const MAX_TRAN_TEXT: usize = 36;

/// The options of `read` and `deploy` that only the source types take.
#[derive(Clone, Copy, Debug, Default)]
pub struct SourceOptions<'a> {
    pub include: Option<IncludeKind>,
    pub fugr: Option<&'a str>,
    pub main: Option<&'a str>,
    pub create_include: bool,
}

/// What a `create` of a DDIC or transaction type was given besides the type and the name.
#[derive(Clone, Copy, Debug, Default)]
pub struct CreateOptions<'a> {
    pub text: Option<&'a str>,
    pub package: Option<&'a str>,
    pub tr: Option<&'a str>,
    pub fugr: Option<&'a str>,
    pub subc: Option<Subc>,
    pub prog: Option<&'a str>,
    pub file: Option<&'a Path>,
}

/// `read <TYPE> <NAME> <OUT>`: a SAP name may be read, a partial read is not offered, and a file
/// is never overwritten without `--force`.
pub fn check_read(
    kind: Kind,
    name: &str,
    out: &Path,
    force: bool,
    lines: Option<LineRange>,
    options: &SourceOptions<'_>,
) -> Result<(), String> {
    check_source_options(options)?;

    if lines.is_some() {
        return Err(format!("--lines does not apply to {}", kind.token()));
    }

    validate_name_within(name, kind.max_name())?;

    if out.exists() && !force {
        return Err(format!(
            "{} already exists; use --force to overwrite",
            out.display()
        ));
    }

    Ok(())
}

/// `deploy [--simulate] <TYPE> <NAME> <FILE>`: a transaction has no deploy.
pub fn check_deploy(
    kind: Kind,
    name: &str,
    tr: Option<&str>,
    options: &SourceOptions<'_>,
) -> Result<(), String> {
    if kind == Kind::Tran {
        return Err("TRAN has no deploy; use create or delete".to_owned());
    }

    check_source_options(options)?;
    check_customer_name(kind, name)?;

    tr.map_or(Ok(()), check_request)
}

/// `create [--simulate] <TYPE> <NAME> ...`: a DDIC object takes its XML FILE, a transaction takes
/// `--prog` and `--text` and no FILE.
pub fn check_create(kind: Kind, name: &str, options: &CreateOptions<'_>) -> Result<(), String> {
    let tran = kind == Kind::Tran;

    if tran != options.prog.is_some() {
        return Err(PROG_RULE.to_owned());
    }

    match (tran, options.file) {
        (true, Some(_)) => return Err("create TRAN does not accept FILE".to_owned()),
        (false, None) => return Err(format!("create {} needs FILE (XML)", kind.token())),
        _ => {}
    }

    if tran && let Some(text) = options.text {
        let length = text.chars().count();

        if length > MAX_TRAN_TEXT {
            return Err(format!(
                "--text is {length} characters; TRAN allows {MAX_TRAN_TEXT}"
            ));
        }
    }

    validate_name_within(name, kind.max_name())?;

    if name.starts_with('/') || !name.starts_with(['Z', 'Y', 'z', 'y']) {
        return Err(format!("a created name must start with Z or Y: {name}"));
    }

    if options.subc.is_some() {
        return Err("--subc only applies to PROG".to_owned());
    }

    if options.fugr.is_some() {
        return Err("--fugr is only valid for FUNC".to_owned());
    }

    check_package_scope(options.package, options.tr)
}

/// The rows of a `delete` batch that name a DDIC or transaction type. A row of another type is
/// checked by the delete plan.
pub fn check_delete(targets: &[DeleteTarget], tr: Option<&str>) -> Result<(), String> {
    for target in targets {
        if let DeleteKind::Runner(kind) = target.kind {
            check_customer_name(kind, &target.name)?;
        }
    }

    tr.map_or(Ok(()), check_request)
}

/// The type of the first DDIC or transaction row of a batch.
pub fn first_kind(targets: &[DeleteTarget]) -> Option<Kind> {
    targets.iter().find_map(|target| match target.kind {
        DeleteKind::Runner(kind) => Some(kind),
        _ => None,
    })
}

/// What a command prints once its grammar passed, until the operation behind it exists.
pub fn pending(operation: &str, kind: Kind) -> String {
    format!("{operation} {} is not implemented yet", kind.token())
}

fn check_source_options(options: &SourceOptions<'_>) -> Result<(), String> {
    if options.include.is_some() {
        return Err("--include is only valid for CLAS".to_owned());
    }

    if options.fugr.is_some() {
        return Err("--fugr is only valid for FUNC".to_owned());
    }

    if options.main.is_some() {
        return Err("--main is only valid for PROG".to_owned());
    }

    if options.create_include {
        return Err("--create-include requires CLAS with --include".to_owned());
    }

    Ok(())
}

/// The name rule of a write or a delete: the grammar of the type, inside the Z and Y scope.
fn check_customer_name(kind: Kind, name: &str) -> Result<(), String> {
    validate_name_within(name, kind.max_name())?;

    if name.starts_with(['Z', 'Y', 'z', 'y']) {
        Ok(())
    } else {
        Err(format!(
            "write and delete accept only Z and Y names: {}",
            name.to_ascii_uppercase()
        ))
    }
}

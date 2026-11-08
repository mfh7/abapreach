//! The M3 object contract: per-kind URIs, limits and the option rules that decide, before any
//! connection is opened, whether a command is valid. Every `Err` is the text of a `STOP`.

use crate::adt::prog_uri;
use crate::cli::{CreateKind, IncludeKind, LineRange, ObjectKind, Subc};
use crate::extension::KindSpec;
use crate::hash::{validate_name_within, validate_source};
use std::path::Path;

/// The package a `create` uses when `--package` is absent.
pub const DEFAULT_PACKAGE: &str = "$TMP";

const MAX_GROUP: usize = 26;
const MAX_PACKAGE: usize = 30;
/// TRKORR is a CHAR 20 field.
const MAX_REQUEST: usize = 20;

/// Where a created object lives. A transportable package always carries its request, so a create
/// POST without `corrNr` cannot be built for it: SAP would generate a request of its own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageScope {
    /// A `$` package: nothing is transported.
    Local,
    Transportable {
        request: String,
    },
}

impl PackageScope {
    /// The scope of `package` with the `--tr` of the command; every mismatch is a `STOP`.
    pub fn of(package: &str, tr: Option<&str>) -> Result<Self, String> {
        // Packages that start with $ are local; nothing in them is transported.
        match (package.starts_with('$'), tr) {
            (true, Some(_)) => Err(format!("--tr is not valid for the local package {package}")),
            (true, None) => Ok(Self::Local),
            (false, None) => Err(format!(
                "package {package} is transportable: --tr is required"
            )),
            (false, Some(request)) => {
                check_request(request)?;

                Ok(Self::Transportable {
                    request: request.to_owned(),
                })
            }
        }
    }
}

/// The request goes into a query string, so only a plain TRKORR is accepted.
pub(crate) fn check_request(request: &str) -> Result<(), String> {
    let valid = !request.is_empty()
        && request.len() <= MAX_REQUEST
        && request.bytes().all(|byte| byte.is_ascii_alphanumeric());

    if valid {
        Ok(())
    } else {
        Err(format!("invalid --tr: {request}"))
    }
}

impl ObjectKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Prog => "PROG",
            Self::Func => "FUNC",
            Self::Clas => "CLAS",
            Self::Intf => "INTF",
        }
    }

    /// The SAP name limit of the type.
    pub fn max_name(self) -> usize {
        match self {
            Self::Prog => 40,
            Self::Func | Self::Clas | Self::Intf => 30,
        }
    }

    /// The URI the lock and the activation use. A class include shares its class here. A FUNC
    /// needs its function group, which the caller resolves from the server or `--fugr`.
    pub fn object_uri(self, name: &str, group: Option<&str>) -> Result<String, String> {
        if self == Self::Prog {
            return Ok(prog_uri(name));
        }

        let name = path_segment(self, name)?;

        match self {
            Self::Clas => Ok(format!("/sap/bc/adt/oo/classes/{name}")),
            Self::Intf => Ok(format!("/sap/bc/adt/oo/interfaces/{name}")),
            _ => {
                let group = group.ok_or("a function module URI needs its function group")?;

                Ok(format!(
                    "/sap/bc/adt/functions/groups/{}/fmodules/{name}",
                    path_segment(self, group)?
                ))
            }
        }
    }

    /// The URI of the source that is read and written: `<object>/source/main`, or the include of
    /// a class.
    pub fn source_uri(
        self,
        name: &str,
        group: Option<&str>,
        include: Option<IncludeKind>,
    ) -> Result<String, String> {
        let object = self.object_uri(name, group)?;

        match (self, include) {
            (_, None) => Ok(format!("{object}/source/main")),
            (Self::Clas, Some(include)) => Ok(format!("{object}/includes/{}", include.segment())),
            (_, Some(_)) => Err("--include is only valid for CLAS".to_owned()),
        }
    }

    /// The URI of the version feed. A class has no `source/main/versions`; its feeds sit under
    /// `includes/<kind>/versions`, with `main` for the main source.
    pub fn versions_uri(
        self,
        name: &str,
        group: Option<&str>,
        include: Option<IncludeKind>,
    ) -> Result<String, String> {
        let object = self.object_uri(name, group)?;

        match (self, include) {
            (Self::Clas, include) => Ok(format!(
                "{object}/includes/{}/versions",
                include.map_or("main", IncludeKind::segment)
            )),
            (_, None) => Ok(format!("{object}/source/main/versions")),
            (_, Some(_)) => Err("--include is only valid for CLAS".to_owned()),
        }
    }
}

impl IncludeKind {
    /// The path segment of the include under `/includes/`.
    pub fn segment(self) -> &'static str {
        match self {
            Self::Ccimp => "implementations",
            Self::Ccdef => "definitions",
            Self::Ccmac => "macros",
            Self::Ccau => "testclasses",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Ccimp => "CCIMP",
            Self::Ccdef => "CCDEF",
            Self::Ccmac => "CCMAC",
            Self::Ccau => "CCAU",
        }
    }
}

impl CreateKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Prog => "PROG",
            Self::Fugr => "FUGR",
            Self::Func => "FUNC",
            Self::Clas => "CLAS",
            Self::Intf => "INTF",
        }
    }

    pub fn max_name(self) -> usize {
        match self {
            Self::Prog => 40,
            Self::Fugr => MAX_GROUP,
            Self::Func | Self::Clas | Self::Intf => 30,
        }
    }

    /// The limit of `--text`, the same as `MAX_TEXT` of the Python tool.
    pub fn max_text(self) -> usize {
        match self {
            Self::Prog => 70,
            Self::Fugr => 40,
            Self::Func => 74,
            Self::Clas | Self::Intf => 60,
        }
    }

    /// The URI the create POST goes to. A FUNC is created inside its function group.
    pub fn creation_uri(self, group: Option<&str>) -> Result<String, String> {
        match self {
            Self::Prog => Err(self.not_created_here()),
            Self::Fugr => Ok("/sap/bc/adt/functions/groups".to_owned()),
            Self::Clas => Ok("/sap/bc/adt/oo/classes".to_owned()),
            Self::Intf => Ok("/sap/bc/adt/oo/interfaces".to_owned()),
            Self::Func => {
                let group = group.ok_or("create FUNC needs its function group")?;

                Ok(format!(
                    "/sap/bc/adt/functions/groups/{}/fmodules",
                    path_segment(ObjectKind::Func, group)?
                ))
            }
        }
    }

    /// The create POST of the scope: a transportable package adds its `corrNr`, which SAP needs
    /// to record the object in that request instead of generating one.
    pub fn creation_request_uri(
        self,
        group: Option<&str>,
        scope: &PackageScope,
    ) -> Result<String, String> {
        let uri = self.creation_uri(group)?;

        Ok(match scope {
            PackageScope::Local => uri,
            PackageScope::Transportable { request } => format!("{uri}?corrNr={request}"),
        })
    }

    /// The metadata URI of the created object, which the reconciliation of an uncertain create
    /// reads. A FUNC needs its function group.
    pub fn object_uri(self, name: &str, group: Option<&str>) -> Result<String, String> {
        if self == Self::Prog {
            return Err(self.not_created_here());
        }

        let kind = match self {
            Self::Fugr | Self::Func => ObjectKind::Func,
            Self::Clas => ObjectKind::Clas,
            Self::Intf => ObjectKind::Intf,
            Self::Prog => unreachable!("returned above"),
        };
        let segment = path_segment(kind, name)?;

        match self {
            Self::Fugr => Ok(format!("/sap/bc/adt/functions/groups/{segment}")),
            Self::Clas => Ok(format!("/sap/bc/adt/oo/classes/{segment}")),
            Self::Intf => Ok(format!("/sap/bc/adt/oo/interfaces/{segment}")),
            Self::Func => {
                let group = group.ok_or("create FUNC needs its function group")?;

                Ok(format!(
                    "/sap/bc/adt/functions/groups/{}/fmodules/{segment}",
                    path_segment(kind, group)?
                ))
            }
            Self::Prog => unreachable!("returned above"),
        }
    }

    /// The read-only validation request of the create. `scope` is the package, or for a FUNC its
    /// function group. Values are percent-encoded; the server parses a namespaced name from
    /// both the encoded and the literal form.
    pub fn validation_uri(self, name: &str, text: &str, scope: &str) -> Result<String, String> {
        let (path, object_type, scope_key) = match self {
            Self::Prog => return Err(self.not_created_here()),
            Self::Fugr => ("/sap/bc/adt/functions/validation", "FUGR/F", "packagename"),
            Self::Func => ("/sap/bc/adt/functions/validation", "FUGR/FF", "fugrname"),
            Self::Clas => (
                "/sap/bc/adt/oo/validation/objectname",
                "CLAS/OC",
                "packagename",
            ),
            Self::Intf => (
                "/sap/bc/adt/oo/validation/objectname",
                "INTF/OI",
                "packagename",
            ),
        };

        Ok(format!(
            "{path}?objtype={}&objname={}&description={}&{scope_key}={}",
            query_value(object_type),
            query_value(&name.to_ascii_uppercase()),
            query_value(text),
            query_value(&scope.to_ascii_uppercase()),
        ))
    }

    /// A `PROG` is created by its own operation, not by the ADT collection URIs of this type.
    fn not_created_here(self) -> String {
        format!("create {} is not available yet", self.token())
    }
}

/// The URI of a program include collection or member.
const INCLUDES_URI: &str = "/sap/bc/adt/programs/includes";

/// The ADT URI of a program include: lowercase name, a namespace `/` becomes `%2f` like a program.
pub fn include_uri(name: &str) -> String {
    format!(
        "{INCLUDES_URI}/{}",
        name.to_ascii_lowercase().replace('/', "%2f")
    )
}

/// `GET` on this lists the main programs of an include.
pub fn mainprograms_uri(name: &str) -> String {
    format!("{}/mainprograms", include_uri(name))
}

/// The URI an include is checked or activated under: the include itself with its main program
/// as the only query, `?context=<encoded URI of the program>`. Nothing else may carry it.
pub fn context_uri(include: &str, main: &str) -> String {
    format!(
        "{}?context={}",
        include_uri(include),
        query_value(&prog_uri(main))
    )
}

/// The URI the create POST of a program or include goes to; a transportable package adds its
/// `corrNr` so that SAP records the object in that request instead of generating one.
pub fn program_creation_request_uri(subc: Subc, scope: &PackageScope) -> String {
    let uri = match subc {
        Subc::Program => "/sap/bc/adt/programs/programs",
        Subc::Include => INCLUDES_URI,
    };

    match scope {
        PackageScope::Local => uri.to_owned(),
        PackageScope::Transportable { request } => format!("{uri}?corrNr={request}"),
    }
}

/// The informational validation of a program or include name. Keys, their order and the raw
/// object type are the recorded exchange; the answer is never a guard.
pub fn program_validation_uri(subc: Subc, name: &str, text: &str, package: &str) -> String {
    let object_type = match subc {
        Subc::Program => "PROG/P",
        Subc::Include => "PROG/I",
    };

    format!(
        "/sap/bc/adt/programs/validation?objname={}&packagename={}&description={}&objtype={object_type}",
        query_value(&name.to_ascii_uppercase()),
        query_value(&package.to_ascii_uppercase()),
        query_value(text),
    )
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
fn query_value(value: &str) -> String {
    let mut encoded = String::new();

    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }

    encoded
}

/// A name as one path segment. Only a PROG has an evidenced path form for a namespaced name, so
/// the other kinds refuse it instead of guessing an encoding.
fn path_segment(kind: ObjectKind, name: &str) -> Result<String, String> {
    segment_of(kind.token(), name)
}

fn segment_of(token: &str, name: &str) -> Result<String, String> {
    if name.starts_with('/') {
        return Err(format!(
            "a namespaced {token} name has no verified source path yet: {name}"
        ));
    }

    Ok(name.to_ascii_lowercase())
}

/// The options that read and deploy share: the kind decides what is allowed.
fn check_scope(
    kind: ObjectKind,
    name: &str,
    include: Option<IncludeKind>,
    fugr: Option<&str>,
) -> Result<(), String> {
    scope_rules(
        Some(kind),
        kind.token(),
        kind.max_name(),
        name,
        include,
        fugr,
    )
}

/// The scope rules for a standard kind (`Some`) or an extension type (`None`, with its own token
/// and name limit): `--include`, `--fugr`, the name and the group.
fn scope_rules(
    kind: Option<ObjectKind>,
    token: &str,
    max_name: usize,
    name: &str,
    include: Option<IncludeKind>,
    fugr: Option<&str>,
) -> Result<(), String> {
    if include.is_some() && kind != Some(ObjectKind::Clas) {
        return Err("--include is only valid for CLAS".to_owned());
    }

    if fugr.is_some() && kind != Some(ObjectKind::Func) {
        return Err("--fugr is only valid for FUNC".to_owned());
    }

    validate_name_within(name, max_name)?;

    if kind != Some(ObjectKind::Prog) {
        segment_of(token, name)?;
    }

    if let Some(group) = fugr {
        check_group(group)?;
    }

    Ok(())
}

/// The `--main` option names the program an include belongs to; it is a `PROG` option only, and
/// its program-vs-include decision happens after URI resolution on the server.
fn check_main(kind: ObjectKind, main: Option<&str>) -> Result<(), String> {
    main_rule(Some(kind), main)
}

fn main_rule(kind: Option<ObjectKind>, main: Option<&str>) -> Result<(), String> {
    let Some(main) = main else {
        return Ok(());
    };

    if kind != Some(ObjectKind::Prog) {
        return Err("--main is only valid for PROG".to_owned());
    }

    validate_name_within(main, ObjectKind::Prog.max_name())
        .map_err(|error| format!("invalid --main: {error}"))
}

fn check_group(group: &str) -> Result<(), String> {
    validate_name_within(group, MAX_GROUP)?;
    path_segment(ObjectKind::Func, group).map(drop)
}

/// The supplied `--fugr` must name the group the server resolved for the function module.
pub fn check_group_match(supplied: Option<&str>, resolved: &str) -> Result<(), String> {
    match supplied {
        Some(group) if !group.eq_ignore_ascii_case(resolved) => Err(format!(
            "--fugr {group} does not match function group {resolved} of the function module"
        )),
        _ => Ok(()),
    }
}

pub fn check_read(
    kind: ObjectKind,
    name: &str,
    out: &Path,
    include: Option<IncludeKind>,
    fugr: Option<&str>,
    lines: Option<LineRange>,
    main: Option<&str>,
) -> Result<(), String> {
    check_scope(kind, name, include, fugr)?;
    check_main(kind, main)?;

    // A partial file must never be deployed as the full source.
    if lines.is_some() && out != Path::new("-") {
        return Err("--lines requires OUT to be '-' (stdout)".to_owned());
    }

    Ok(())
}

/// The common local rules of `read` for an extension type, from its spec; the extension adds its
/// own after this.
pub fn check_read_extension(
    spec: &KindSpec,
    name: &str,
    out: &Path,
    include: Option<IncludeKind>,
    fugr: Option<&str>,
    lines: Option<LineRange>,
    main: Option<&str>,
) -> Result<(), String> {
    scope_rules(None, spec.token, spec.max_name, name, include, fugr)?;
    main_rule(None, main)?;

    if lines.is_some() && out != Path::new("-") {
        return Err("--lines requires OUT to be '-' (stdout)".to_owned());
    }

    Ok(())
}

pub fn check_deploy_options(
    kind: ObjectKind,
    name: &str,
    include: Option<IncludeKind>,
    create_include: bool,
    fugr: Option<&str>,
    main: Option<&str>,
) -> Result<(), String> {
    check_scope(kind, name, include, fugr)?;
    check_main(kind, main)?;

    if create_include && (kind != ObjectKind::Clas || include.is_none()) {
        return Err("--create-include requires CLAS with --include".to_owned());
    }

    Ok(())
}

/// The deploy file: name and line rules of the type, and for a FUNC the ADT format.
pub fn check_deploy_source(kind: ObjectKind, name: &str, lines: &[String]) -> Result<(), String> {
    validate_source(name, kind.max_name(), lines)?;

    if kind != ObjectKind::Func {
        return Ok(());
    }

    match lines.iter().position(|line| line.starts_with("*\"")) {
        Some(index) => Err(format!(
            "line {} starts with *\" : a FUNC file uses the ADT format ('FUNCTION <name> \
             IMPORTING ... .' followed by the body), not the include format with a *\" header",
            index + 1
        )),
        None => Ok(()),
    }
}

/// `check_create` takes every option the grammar allows; the alternatives, a struct of options,
/// would hide which option a caller supplied.
#[allow(clippy::too_many_arguments)]
pub fn check_create(
    kind: CreateKind,
    name: &str,
    text: &str,
    package: Option<&str>,
    fugr: Option<&str>,
    tr: Option<&str>,
    subc: Option<Subc>,
    misuse: Option<&str>,
    file: Option<&Path>,
) -> Result<(), String> {
    let subject = CreateSubject {
        kind: Some(kind),
        token: kind.token(),
        max_name: kind.max_name(),
        max_text: kind.max_text(),
    };

    create_rules(
        &subject,
        name,
        text,
        package,
        fugr,
        tr,
        subc,
        misuse,
        &|| Ok(()),
        file,
    )
}

/// The common local rules of `create` for an extension type, from its spec, in the order of the
/// standard rules. `own` runs where the extension's own rules belong: after the option rules and
/// before the FILE rules.
#[allow(clippy::too_many_arguments)]
pub fn check_create_extension(
    spec: &KindSpec,
    name: &str,
    text: &str,
    package: Option<&str>,
    fugr: Option<&str>,
    tr: Option<&str>,
    subc: Option<Subc>,
    misuse: Option<&str>,
    own: &dyn Fn() -> Result<(), String>,
    file: Option<&Path>,
) -> Result<(), String> {
    let subject = CreateSubject {
        kind: None,
        token: spec.token,
        max_name: spec.max_name,
        max_text: spec.max_text,
    };

    create_rules(
        &subject, name, text, package, fugr, tr, subc, misuse, own, file,
    )
}

/// What the create rules need to know about a type: a standard kind (`Some`) or an extension
/// type (`None`) with its token and limits.
struct CreateSubject<'a> {
    kind: Option<CreateKind>,
    token: &'a str,
    max_name: usize,
    max_text: usize,
}

#[allow(clippy::too_many_arguments)]
fn create_rules(
    subject: &CreateSubject<'_>,
    name: &str,
    text: &str,
    package: Option<&str>,
    fugr: Option<&str>,
    tr: Option<&str>,
    subc: Option<Subc>,
    misuse: Option<&str>,
    own: &dyn Fn() -> Result<(), String>,
    file: Option<&Path>,
) -> Result<(), String> {
    let kind = subject.kind;

    validate_name_within(name, subject.max_name)?;

    if name.starts_with('/') || !matches!(name.as_bytes()[0].to_ascii_uppercase(), b'Z' | b'Y') {
        return Err(format!("a created name must start with Z or Y: {name}"));
    }

    let length = text.chars().count();

    if length > subject.max_text {
        return Err(format!(
            "--text is {length} characters; {} allows {}",
            subject.token, subject.max_text
        ));
    }

    match (kind, fugr) {
        (Some(CreateKind::Func), None) => {
            return Err("create FUNC requires --fugr <FG>".to_owned());
        }
        (Some(CreateKind::Func), Some(group)) => check_group(group)?,
        (_, Some(_)) => return Err("--fugr is only valid for FUNC".to_owned()),
        _ => {}
    }

    if subc.is_some() && kind != Some(CreateKind::Prog) {
        return Err("--subc only applies to PROG".to_owned());
    }

    // The text of an option that belongs to another kind and was given with this one.
    if let Some(message) = misuse {
        return Err(message.to_owned());
    }

    own()?;

    if file.is_some() {
        if kind == Some(CreateKind::Prog) && subc == Some(Subc::Include) {
            return Err(
                "new include: deploy the source after the INCLUDE statement exists in the \
                 parent program (create PROG --subc I without FILE)"
                    .to_owned(),
            );
        }

        if kind != Some(CreateKind::Prog) {
            return Err(format!("create {} does not accept FILE", subject.token));
        }
    }

    // A FUNC takes the package of its function group, which only the server knows.
    if kind == Some(CreateKind::Func) {
        return match package {
            Some(_) => Err(
                "--package is not valid for create FUNC: the function module uses the \
                            package of its function group"
                    .to_owned(),
            ),
            None => tr.map_or(Ok(()), check_request),
        };
    }

    check_package_scope(package, tr)
}

/// `--package` (default `$TMP`) and `--tr` of a create: the package grammar and the rule that a
/// transportable package needs a request and a local one takes none.
pub(crate) fn check_package_scope(package: Option<&str>, tr: Option<&str>) -> Result<(), String> {
    let package = package.unwrap_or(DEFAULT_PACKAGE);
    let valid = !package.is_empty()
        && package.chars().count() <= MAX_PACKAGE
        && package.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '/' | '$')
        });

    if !valid {
        return Err(format!("invalid --package: {package}"));
    }

    PackageScope::of(package, tr).map(drop)
}

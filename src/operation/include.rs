//! What a `PROG` name is on the server, and the main program an include is checked and activated
//! under. The URI of the answering object is used for every later request of the operation;
//! nothing is guessed. Read, deploy and the later create, delete and MCP tasks share this unit.

use crate::adt::{parse_inactive_feed_refs, prog_uri, versions_uri};
use crate::object::{include_uri, mainprograms_uri};
use crate::transport::{Request, Response, SadtTransport, TransportError};

/// Whether a `PROG` name is a program (`subc 1`) or an include (`subc I`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgKind {
    Program,
    Include,
}

/// The object a `PROG` name resolved to: its kind and the object URI every later request uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgTarget {
    pub kind: ProgKind,
    pub uri: String,
}

impl ProgTarget {
    pub fn program(name: &str) -> Self {
        Self {
            kind: ProgKind::Program,
            uri: prog_uri(name),
        }
    }

    pub fn include(name: &str) -> Self {
        Self {
            kind: ProgKind::Include,
            uri: include_uri(name),
        }
    }

    pub fn is_include(&self) -> bool {
        self.kind == ProgKind::Include
    }

    pub fn source_uri(&self) -> String {
        format!("{}/source/main", self.uri)
    }

    pub fn versions_uri(&self) -> String {
        versions_uri(&self.uri)
    }
}

/// Why a resolution step ended; the caller turns it into its own failure class.
#[derive(Debug)]
pub enum ResolveError {
    Transport(TransportError),
    /// A request answered a status that has no rule here.
    Status {
        what: String,
        status: u16,
    },
    /// A rule of the command that the server state breaks (exit 2).
    Stop(String),
}

#[derive(Debug)]
pub enum Located {
    Found(ProgTarget),
    /// Neither `programs/programs` nor `programs/includes` knows the name.
    NotFound,
}

/// `GET` of the object properties: `programs/programs/<n>`, then `programs/includes/<n>`. The
/// first URI that answers 200 wins; both 404 is not found. Any other status is an error, because
/// the kind of the object is then unknown.
pub fn locate(transport: &mut dyn SadtTransport, name: &str) -> Result<Located, ResolveError> {
    Ok(match locate_properties(transport, name)? {
        Some((target, _)) => Located::Found(target),
        None => Located::NotFound,
    })
}

/// Like [`locate`], and keeps the 200 answer: its body is the object properties, so a caller that
/// reads them anyway does not ask twice.
pub fn locate_properties(
    transport: &mut dyn SadtTransport,
    name: &str,
) -> Result<Option<(ProgTarget, Response)>, ResolveError> {
    for target in [ProgTarget::program(name), ProgTarget::include(name)] {
        let response = get(transport, &target.uri, "application/*")?;

        match response.status {
            200 => return Ok(Some((target, response))),
            404 => {}
            status => {
                return Err(ResolveError::Status {
                    what: format!("cannot resolve PROG {}", name.to_ascii_uppercase()),
                    status,
                });
            }
        }
    }

    Ok(None)
}

/// The main program an include is checked and activated under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MainProgram {
    /// Upper case.
    pub name: String,
    /// The include has no indexed main program yet and `--main` supplied one: the first
    /// activation under it creates the index, so the check before it cannot run.
    pub first_activation: bool,
}

/// Picks the main program of `include` from the listed ones and the `--main` of the command.
/// `--main` must be listed when anything is listed; without it exactly one listed program is
/// used. Every `Err` is the text of a `STOP`.
pub fn choose_main(
    include: &str,
    listed: &[String],
    given: Option<&str>,
) -> Result<MainProgram, String> {
    let include = include.to_ascii_uppercase();

    match given {
        Some(main) => {
            let main = main.to_ascii_uppercase();

            if !listed.is_empty() && !listed.contains(&main) {
                return Err(format!(
                    "main program {main} is not a main program of include {include} (listed: {})",
                    listed.join(", ")
                ));
            }

            Ok(MainProgram {
                name: main,
                first_activation: listed.is_empty(),
            })
        }
        None => match listed {
            [] => Err(format!(
                "include {include} has no main program; pass --main <PROG>"
            )),
            [only] => Ok(MainProgram {
                name: only.clone(),
                first_activation: false,
            }),
            _ => Err(format!(
                "include {include} has several main programs ({}); pass --main <PROG>",
                listed.join(", ")
            )),
        },
    }
}

/// The program names of a `GET <include>/mainprograms` answer, upper case and each once. A
/// reference that is not the URI of a program of that name is an error: a main program that is
/// not a program cannot be used as the context.
pub fn parse_main_programs(xml: &str) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();

    for reference in parse_inactive_feed_refs(xml)? {
        let name = reference.name.to_ascii_uppercase();

        if name.is_empty() || !reference.uri.eq_ignore_ascii_case(&prog_uri(&name)) {
            return Err(format!(
                "unsupported main program reference {}",
                reference.uri
            ));
        }

        if !names.contains(&name) {
            names.push(name);
        }
    }

    Ok(names)
}

/// Whether `source` holds the statement `INCLUDE <include>.`: case-insensitive, comments
/// (`*` in column 1, `"` to the end of the line) and literals ignored.
pub fn includes_statement(source: &[String], include: &str) -> bool {
    statements(source).iter().any(|tokens| {
        matches!(
            tokens.as_slice(),
            [keyword, name]
                if keyword.eq_ignore_ascii_case("INCLUDE") && name.eq_ignore_ascii_case(include)
        )
    })
}

/// The complete statements of an ABAP source as lists of blank-separated tokens. A literal stays
/// one token, so a period inside it ends nothing.
fn statements(source: &[String]) -> Vec<Vec<String>> {
    let mut done = Vec::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut word = String::new();

    for line in source {
        if line.starts_with('*') {
            continue;
        }

        let mut quote: Option<char> = None;
        let mut characters = line.chars();

        while let Some(character) = characters.next() {
            if let Some(open) = quote {
                word.push(character);

                if character == '\\' && open == '|' {
                    characters.next().inspect(|escaped| word.push(*escaped));
                } else if character == open {
                    quote = None;
                }

                continue;
            }

            match character {
                '"' => break,
                '\'' | '`' | '|' => {
                    quote = Some(character);
                    word.push(character);
                }
                '.' => {
                    flush(&mut word, &mut tokens);
                    done.push(std::mem::take(&mut tokens));
                }
                blank if blank.is_whitespace() => flush(&mut word, &mut tokens),
                other => word.push(other),
            }
        }

        flush(&mut word, &mut tokens);
    }

    done
}

fn flush(word: &mut String, tokens: &mut Vec<String>) {
    if !word.is_empty() {
        tokens.push(std::mem::take(word));
    }
}

/// The main program of `include`: the listed ones from the server, then the checks of a
/// `--main`: it must be a program that exists (resolved through `programs/programs`, never as an
/// include) whose active source holds the `INCLUDE` statement.
pub fn resolve_main(
    transport: &mut dyn SadtTransport,
    include: &str,
    given: Option<&str>,
) -> Result<MainProgram, ResolveError> {
    let upper = include.to_ascii_uppercase();
    let response = get(transport, &mainprograms_uri(include), "application/*")?;

    if response.status != 200 {
        return Err(ResolveError::Status {
            what: format!("cannot read the main programs of include {upper}"),
            status: response.status,
        });
    }

    let listed =
        parse_main_programs(&String::from_utf8_lossy(&response.body)).map_err(|error| {
            ResolveError::Stop(format!(
                "cannot read the main programs of include {upper}: {error}"
            ))
        })?;
    let main = choose_main(include, &listed, given).map_err(ResolveError::Stop)?;

    if given.is_some() {
        verify_main(transport, include, &main.name)?;
    }

    Ok(main)
}

/// The `--main` program exists as a program and its active source includes `include`.
fn verify_main(
    transport: &mut dyn SadtTransport,
    include: &str,
    main: &str,
) -> Result<(), ResolveError> {
    let program = ProgTarget::program(main);
    let properties = get(transport, &program.uri, "application/*")?;

    match properties.status {
        200 => {}
        404 => {
            return Err(ResolveError::Stop(format!(
                "main program {main} does not exist as a program"
            )));
        }
        status => {
            return Err(ResolveError::Status {
                what: format!("cannot resolve main program {main}"),
                status,
            });
        }
    }

    let source = get(
        transport,
        &format!("{}?version=active", program.source_uri()),
        "text/plain",
    )?;

    if source.status != 200 {
        return Err(ResolveError::Status {
            what: format!("cannot read main program {main}"),
            status: source.status,
        });
    }

    let text = String::from_utf8_lossy(&source.body);
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();

    if includes_statement(&lines, include) {
        Ok(())
    } else {
        Err(ResolveError::Stop(format!(
            "main program {main} does not contain the statement INCLUDE {}.",
            include.to_ascii_uppercase()
        )))
    }
}

fn get(
    transport: &mut dyn SadtTransport,
    uri: &str,
    accept: &str,
) -> Result<Response, ResolveError> {
    let mut request = Request::get(uri);

    request
        .headers
        .insert("Accept".to_owned(), accept.to_owned());

    let response = transport
        .request(request)
        .map_err(ResolveError::Transport)?;

    // A refused logon ends the operation here, whatever the request was.
    match response.authorization_failure() {
        Some(error) => Err(ResolveError::Transport(error)),
        None => Ok(response),
    }
}

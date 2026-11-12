//! The MCP tools for the DDIC types (`DTEL`, `DOMA`, `TABL`, `TTYP`) and `TRAN` (M6 Task 26).
//!
//! These types run through a temporary class in `$TMP` (see `operation/runner.rs`), so every
//! operation, `read` and the simulations included, is more than a read: it takes the single
//! in-flight slot, and it is refused when the connection forbids MCP writes. The tools call the
//! same operations as the command line; this module only adapts arguments and results and binds
//! the plan token.

use super::{
    DdicPreview, Deadline, McpTools, Operation, Settled, Ticket, WithCancel, WriteGate, ask, bind,
    parse,
};
use crate::app::Exit;
use crate::cli::{DeleteKind, DeleteTarget, RUNNER_KINDS, Subc};
use crate::config::Connection;
use crate::domain::Status;
use crate::hash::source_hash;
use crate::mcp::{Session, ToolResult};
use crate::mcp_read::{SECRET_REJECTED, holds_secret, optional, required};
use crate::operation::create_ddic::{self, DdicCreate};
use crate::operation::delete::sha256_hex;
use crate::operation::deploy_ddic::{self, DdicDeploy, Preview};
use crate::operation::payload::Kind;
use crate::operation::read_ddic::{self, DdicRead};
use crate::operation::runner_route::random_nonce;
use crate::operation::runner_tran::{self, TranCreate, TranRead};
use crate::operation::simulate_ddic::{self, DdicSimulate, Simulation};
use crate::policy::McpWrite;
use crate::runner_cli::{self, CreateOptions, SourceOptions};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

/// The most DDIC or transaction rows one MCP call handles. One row is one class cycle (about 25 s
/// on the slowest path), so the 55 s deadline of a call, which is only looked at between rows,
/// leaves room for one. Task 26A fixes the final number from the measured timings.
pub(super) const MAX_RUNNER_ROWS: usize = 1;

/// What a reader that asks while a DDIC deploy waits for its human is told.
const BUSY_WAITING: &str =
    "STOP: busy: a DDIC deploy is waiting for its confirmation; read again afterwards";

/// The DDIC or transaction type a call names, if it names one.
pub(super) fn runner_type_of(arguments: &Map<String, Value>) -> Option<Kind> {
    let token = arguments.get("type")?.as_str()?;

    RUNNER_KINDS
        .iter()
        .find(|kind| kind.token() == token)
        .copied()
}

/// An option these types do not take is refused, never ignored. `false` is the same as absent.
fn reject_options(arguments: &Map<String, Value>, kind: Kind, keys: &[&str]) -> Result<(), String> {
    for key in keys {
        if !matches!(
            arguments.get(*key),
            None | Some(Value::Null) | Some(Value::Bool(false))
        ) {
            return Err(format!("{key} does not apply to {}", kind.token()));
        }
    }

    Ok(())
}

/// The local rules of the DDIC and transaction rows of a delete batch, and how many there are.
pub(super) fn check_delete_rows(
    targets: &[DeleteTarget],
    tr: Option<&str>,
) -> Result<usize, String> {
    let rows = targets
        .iter()
        .filter(|target| matches!(target.kind, DeleteKind::Runner(_)))
        .count();

    if rows == 0 {
        return Ok(0);
    }

    if targets
        .iter()
        .any(|target| matches!(target.kind, DeleteKind::Extension(_)))
    {
        return Err(
            "a DTEL, DOMA, TABL, TTYP or TRAN row cannot share a call with an extension type"
                .to_owned(),
        );
    }

    if rows > MAX_RUNNER_ROWS {
        return Err(format!(
            "a call takes at most {MAX_RUNNER_ROWS} DTEL, DOMA, TABL, TTYP or TRAN object over \
             MCP; delete the others in further calls"
        ));
    }

    runner_cli::check_delete(targets, tr)?;

    Ok(rows)
}

/// The status of an operation of these types, from the status line it prints. The operations
/// return their text only, so this reads a closed set of exact tokens.
/// shortcut: the text is the only record of the status, upgrade when the operations return it.
fn runner_status(exit: &Exit) -> Option<Status> {
    exit.stdout.lines().rev().find_map(|line| {
        let line = line
            .strip_prefix("CREATE_SIM: ")
            .or_else(|| line.strip_prefix("CREATE: "))
            .unwrap_or(line);
        let leads = |token: &str| {
            line == token
                || line
                    .strip_prefix(token)
                    .is_some_and(|rest| rest.starts_with(':') || rest.starts_with(" ("))
        };

        if line == "SIMULATED_OK" {
            Some(Status::SimulatedOk)
        } else if leads("ACTIVE") {
            Some(Status::Active)
        } else if leads("INACTIVE") {
            Some(Status::Inactive)
        } else if leads("REJECTED") {
            Some(Status::Rejected)
        } else if leads("PARTIAL") {
            Some(Status::Partial)
        } else {
            None
        }
    })
}

/// The live hash that run 1 of a simulate printed: the base of the deploy it earns.
fn live_hash(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("live hash "))
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned)
}

/// A private directory for the file a read writes, removed with the value.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("abapreach-read-{}", random_nonce()));
        // Only the unix mode below needs `mut`.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();

        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;

            builder.mode(0o700);
        }

        builder
            .create(&path)
            .map_err(|error| format!("cannot create a scratch directory: {error}"))?;

        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl<G: WriteGate> McpTools<'_, G> {
    /// `abap_read_source` of a DDIC or transaction type. It creates and deletes a temporary
    /// class, so it is refused where MCP writes are off; under `confirm` and `token` it runs
    /// without a question and without a token, because nothing outlives it.
    pub(super) fn runner_read(
        &mut self,
        arguments: &Map<String, Value>,
        session: &mut dyn Session,
    ) -> Result<ToolResult, String> {
        let kind = runner_type_of(arguments).ok_or("type must be a DDIC or transaction type")?;
        let connection = self.read.connection(arguments)?;

        if connection.policies.mcp_write == McpWrite::Off {
            return Err(format!(
                "reading {} needs a temporary class; mcp_write is off for {}",
                kind.token(),
                connection.id
            ));
        }

        let name = required(arguments, "name")?;

        reject_options(arguments, kind, &["lines", "include", "fugr", "main"])?;

        let scratch = Scratch::new()?;
        let out = scratch.0.join("read.xml");

        runner_cli::check_read(kind, name, &out, false, None, &SourceOptions::default())?;

        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: session.cancel(),
            opened: None,
        };
        let exit = if kind == Kind::Tran {
            runner_tran::read(
                &connection,
                None,
                &TranRead { name, out: &out },
                &mut factories,
            )
        } else {
            read_ddic::read(
                &connection,
                None,
                &DdicRead {
                    kind,
                    name,
                    out: &out,
                    force: false,
                },
                &mut factories,
            )
        };

        drop(factories);

        // The operation reports the scratch file; the data goes into the result instead.
        let shown = out.display().to_string();
        let exit = Exit {
            stdout: exit
                .stdout
                .lines()
                .filter(|line| !line.contains(&shown) && !line.starts_with("hash "))
                .collect::<Vec<_>>()
                .join("\n"),
            ..exit
        };
        let rows: Option<Vec<String>> = (exit.code == 0)
            .then(|| fs::read_to_string(&out).ok())
            .flatten()
            .map(|text| text.lines().map(str::to_owned).collect());
        let status = rows.as_ref().map(|_| Status::Active);
        let mut result = self.finish(&connection, &exit, status, None, json!({}), true, None);

        let Some(rows) = rows else {
            return Ok(result);
        };

        let xml = rows.join("\n");

        if holds_secret(&connection, &xml) {
            return Ok(ToolResult::error(SECRET_REJECTED.to_owned()));
        }

        let hash = source_hash(&rows);

        result.text = format!("{xml}\n{} lines\nhash {hash}\n{}", rows.len(), result.text)
            .trim_end()
            .to_owned();
        result.structured["partial"] = json!(false);
        result.structured["source"] = json!(xml);
        result.structured["lines"] = json!(rows.len());
        result.structured["hash"] = json!(hash);

        Ok(result)
    }

    /// `abap_simulate` and `abap_deploy` of a DDIC type. A deploy is two runs: run 1 shows the
    /// diff and the live hash, and under `confirm` the human is asked with that diff before run 2
    /// writes anything.
    pub(super) fn runner_deploy(
        &mut self,
        connection: Connection,
        kind: Kind,
        arguments: &Map<String, Value>,
        execute: bool,
        session: &mut dyn Session,
        deadline: Deadline,
    ) -> Result<ToolResult, String> {
        let name = required(arguments, "name")?;
        let tr = optional(arguments, "tr")?;

        reject_options(
            arguments,
            kind,
            &["lines", "include", "fugr", "main", "create_include"],
        )?;
        // A transaction has no deploy: this refuses it, and checks the name and the request.
        runner_cli::check_deploy(kind, name, tr, &SourceOptions::default())?;

        let xml = required(arguments, "source")?;
        let upper = name.to_ascii_uppercase();
        let request = DdicSimulate {
            kind,
            name: &upper,
            xml,
            tr,
            simulation: Simulation::Deploy { base: None },
        };

        simulate_ddic::check(&request, &upper)?;

        let fields = bind([
            ("type", Some(kind.token().to_owned())),
            ("name", Some(upper.clone())),
            ("source_sha256", Some(sha256_hex(xml))),
            ("tr", tr.map(str::to_ascii_uppercase)),
        ]);
        let ticket = Ticket::new(Operation::Deploy, connection.id.clone(), fields);
        let admitted = if execute {
            Some(self.admit(&connection, arguments, &ticket, session)?)
        } else {
            None
        };
        let cancel = session.cancel();

        Self::proceed(&deadline, &cancel, self.limits)?;

        let (registry, timeout) = (self.registry, self.limits.elicit);
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let mut declined = None;
        let exit = match &admitted {
            Some((stored, confirm)) => {
                let write = DdicDeploy {
                    kind,
                    name: &upper,
                    xml,
                    tr,
                    base: stored.base.as_deref(),
                };
                let mut gate = |preview: &Preview<'_>| -> Result<(), String> {
                    if !confirm {
                        return Ok(());
                    }

                    let mut shown = stored.clone();

                    shown.ddic = Some(DdicPreview::of(preview.runner, preview.printed));

                    ask(
                        &connection,
                        &shown,
                        registry,
                        timeout,
                        session,
                        &mut |_, _| ToolResult::error(BUSY_WAITING.to_owned()),
                    )
                    .inspect_err(|message| declined = Some(message.clone()))
                };

                deploy_ddic::deploy_gated(
                    &connection,
                    stored.backend,
                    &write,
                    &mut factories,
                    &mut gate,
                )
            }
            None => simulate_ddic::simulate(&connection, None, &request, &mut factories),
        };
        let opened = factories.opened;

        drop(factories);

        if let Some(message) = declined {
            return Err(message);
        }

        let status = runner_status(&exit);
        let issue =
            (!execute && exit.code == 0 && status == Some(Status::SimulatedOk)).then(|| Ticket {
                backend: opened,
                base: live_hash(&exit.stdout),
                ..ticket
            });

        Ok(self.finish(
            &connection,
            &exit,
            status,
            issue,
            json!({}),
            true,
            execute.then(|| Settled::new("abap_deploy", name, &cancel, session)),
        ))
    }

    /// `abap_create_simulate` and `abap_create` of a DDIC type or `TRAN`.
    pub(super) fn runner_create(
        &mut self,
        connection: Connection,
        kind: Kind,
        arguments: &Map<String, Value>,
        execute: bool,
        session: &mut dyn Session,
        mut deadline: Deadline,
    ) -> Result<ToolResult, String> {
        let name = required(arguments, "name")?;
        let text = optional(arguments, "text")?;
        let package = optional(arguments, "package")?;
        let fugr = optional(arguments, "fugr")?;
        let tr = optional(arguments, "tr")?;
        let prog = optional(arguments, "prog")?;
        let source = optional(arguments, "source")?;
        let subc = match optional(arguments, "subc")? {
            Some(subc) => Some(parse::<Subc>(subc, "subc must be one of 1, I")?),
            None => None,
        };
        let tran = kind == Kind::Tran;

        match (tran, prog, source, text) {
            (true, None, _, _) => return Err("prog is required for TRAN".to_owned()),
            (true, _, Some(_), _) => return Err("source does not apply to TRAN".to_owned()),
            (true, _, _, None) => return Err("text is required for TRAN".to_owned()),
            (false, Some(_), _, _) => return Err("prog only applies to TRAN".to_owned()),
            (false, _, None, _) => {
                return Err(format!("source (the XML) is required for {}", kind.token()));
            }
            _ => {}
        }

        let options = CreateOptions {
            text,
            package,
            tr,
            fugr,
            subc,
            prog,
            file: source.map(|_| Path::new("source")),
        };

        runner_cli::check_create(kind, name, &options)?;

        let upper = name.to_ascii_uppercase();
        let simulation = Simulation::Create { package, text };

        if let Some(xml) = source {
            let request = DdicSimulate {
                kind,
                name: &upper,
                xml,
                tr,
                simulation,
            };

            simulate_ddic::check(&request, &upper)?;
        }

        let fields = bind([
            ("type", Some(kind.token().to_owned())),
            ("name", Some(upper.clone())),
            ("text", text.map(str::to_owned)),
            ("package", package.map(str::to_ascii_uppercase)),
            ("tr", tr.map(str::to_ascii_uppercase)),
            ("prog", prog.map(str::to_ascii_uppercase)),
            ("source_sha256", source.map(sha256_hex)),
        ]);
        let ticket = Ticket::new(Operation::Create, connection.id.clone(), fields);
        let stored = if execute {
            Some(self.authorize(&connection, arguments, &ticket, session, &mut deadline)?)
        } else {
            None
        };
        let cancel = session.cancel();

        Self::proceed(&deadline, &cancel, self.limits)?;

        let explicit = stored.as_ref().and_then(|stored| stored.backend);
        let _write = execute.then(|| cancel.hold_write());
        let mut factories = WithCancel {
            inner: self.read.factories(),
            cancel: cancel.clone(),
            opened: None,
        };
        let exit = match (prog, source) {
            (Some(report), _) => {
                let request = TranCreate {
                    name,
                    report,
                    text,
                    package,
                    tr,
                    simulate: !execute,
                };

                runner_tran::create(&connection, explicit, &request, &mut factories)
            }
            (None, Some(xml)) if execute => {
                let request = DdicCreate {
                    kind,
                    name,
                    xml,
                    package,
                    text,
                    tr,
                };

                create_ddic::create(&connection, explicit, &request, &mut factories)
            }
            (None, Some(xml)) => {
                let request = DdicSimulate {
                    kind,
                    name: &upper,
                    xml,
                    tr,
                    simulation,
                };

                simulate_ddic::simulate(&connection, None, &request, &mut factories)
            }
            (None, None) => unreachable!("the checks above need a report or an XML"),
        };
        let opened = factories.opened;

        drop(factories);

        let status = runner_status(&exit);
        let issue =
            (!execute && exit.code == 0 && status == Some(Status::SimulatedOk)).then_some(Ticket {
                backend: opened,
                ..ticket
            });

        Ok(self.finish(
            &connection,
            &exit,
            status,
            issue,
            json!({}),
            true,
            execute.then(|| Settled::new("abap_create", name, &cancel, session)),
        ))
    }
}

# Changelog

All notable changes to ABAPReach are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

**Current Rust CLI:** version `0.15.0` supports `PROG`, `FUNC`, `CLAS` (including local
includes) and `INTF` objects, and program includes, and adds `read-table`, `delete` and the MCP
server. Earlier versions supported `PROG` only.
Versions `0.1.0` to `0.10.0` below describe the earlier Python implementation,
not features available in the current Rust CLI. Their tags live in the
internal repository where that implementation was developed. See the
[roadmap](docs/roadmap.md) for the path to 1.0.0, which brings all of them to Rust.

## [Unreleased]

Version metadata is `0.17.0`. This entry is finalized before the last release candidate.

### Added
- `inactive [--all]` lists the inactive objects of the logon user (`<ADT type> <name> <user>`, an
  include also names its main program); MCP tool `abap_inactive`. The reply shape of the list is
  assumed from the ADT format and not yet probed live; `--all` sends `?USERNAME=*`. An entry marked
  deleted ends with ` deleted`, and `activate` refuses it. The owner of an entry in the flat 7.40
  feed comes from the version feed, never from `adtcore:responsible`.
- `activate [--simulate] TYPE/SUBTYPE:NAME...` activates own inactive Z/Y objects (`PROG/P`,
  `PROG/I`, `CLAS/OC`, `INTF/OI`, `FUGR/FF`), one activation request per object with the worklist
  resend of a transportable class, and proves the result against the inactive list. It refuses a
  productive client (`T000`), `FUGR/F` and every foreign or unlisted object before any request. MCP
  tools `abap_activate_simulate` and `abap_activate` (plan token, `mcp_write`).
- `aunit PROG|CLAS|FUGR NAME` and `aunit --package P` / `--tr TRKORR` run ABAP Unit (harmless
  tests, every duration) and print `<kind> <level> <prog> <class>-><method>: <text>`; exit 0 only
  without alerts. It refuses a productive client and a policy `run: off`. MCP tool `abap_aunit` sits
  behind the policy `mcp_run: confirm` (default `off`) and `mcp_run_allow`: every object of the call
  (also each object a `--package` or `--tr` batch resolves to) must be on the list as `TYPE:NAME`,
  otherwise the whole call is refused before any test runs, naming the missing objects. A batch of
  more than 60 objects is refused (the confirmation form lists 60; the CLI keeps 100). The human
  confirms every call. The reply shape of the run is assumed from the ADT format and not yet probed live.
- RFC admission: the ABAP Unit run configuration (harmless tests, one object) and
  `inactiveobjects?USERNAME=`.
- Fault-injection builds accept `hold-post:<ms>` (pause after the classrun POST, marker
  `FAULT: classrun POST done, pausing N ms`), so the live Ctrl-C cells land after the POST on a slow
  system.
- `run CLAS <class>` (0.17.0): executes an existing class that implements `IF_OO_ADT_CLASSRUN`
  and prints its reply as is. `run` is not a dry run. `run --simulate --plan <FILE>` reads the
  class (exists, active, interface, package, author, active source hash) and the client role and
  writes the plan file; the run needs that file under the default policy and stops when the
  arguments or the live facts differ. Only `Z` and `Y` names run, `--allow-standard` allows a
  standard class; a productive client (`T000-CCCATEGORY = P`) refuses every run. The request is
  sent once from a new session; `--timeout` (default 600, at most 3600) bounds it over ADT HTTP
  and is refused on RFC. Outcomes: reply and exit 0, `RUN_FAILED: runtime error; see ST22
  (dumps)`, `RUN_UNKNOWN: the reply was lost; the code may have run`, `REJECTED`, `STOP`. The
  first Ctrl-C prints `I: run in progress on the server; waiting for the reply (Ctrl-C again to
  leave)` on stderr and waits for the reply, the second leaves with exit 130. A temporary runner
  class (the `runner_prefix` of the connection or `YCL_ABAPREACH_RUN_`, plus exactly eight characters
  of `0-9A-Z`) is never run. The plan binds the main source of a class or report, not the
  includes it calls.
- `run PROG <report>` (0.17.0): submits an active executable report inside the temporary runner
  class (`SUBMIT ... EXPORTING LIST TO MEMORY AND RETURN`, new runner mode `RUN_PROG`) and prints
  its list. `--param NAME=VALUE` (parameters and single-value select-options, values up to 45
  characters) and `--variant` are checked against the selection screen; the plan binds the report
  name, its source hash, `SUBC`, package and every option, and the class compares the active source
  hash again right before `SUBMIT` (`REJECTED: report changed since the simulate`). The class
  checks `S_PROGRAM` (`SUBMIT`) and `TRDIR-SUBC = 1`. `run PROG` and `run SNIPPET` work on
  development systems only: they stop before any change on a system or client that forbids
  repository changes.
- ALV capture for `run PROG`: a grid the report shows (`CL_SALV_TABLE`,
  `REUSE_ALV_GRID_DISPLAY`) is captured with `CL_SALV_BS_RUNTIME_INFO` and printed as tab-separated
  lines (a header of the column texts, then one line per row); only the visible columns of the field
  catalog by default, `--alv-columns all` for every column (CLI only); amounts and quantities are
  written with their currency and unit decimals; `--alv off` keeps the plain list. One grid per run
  is captured.
- `run SNIPPET <file>` (0.17.0): the statements of the file become the body of a method of the
  runner class; the only output is `print( iv_text )`. A snippet that could leave its method
  (`METHOD`, `ENDMETHOD`, `METHODS`, `CLASS`, `ENDCLASS`, `INTERFACE`, `ENDINTERFACE`, `FORM`,
  `ENDFORM`, `DEFINE`, `END-OF-DEFINITION`, `INCLUDE`, judged per statement, colon chains
  included), a statement left open, a control character or a line over 255 characters is refused
  before any connection. A compile error is `SNIPPET_ERROR: line <n>: <text>` (exit 2) with the
  line of the file; the snippet text and its hash are part of the plan.
- Outcomes of a report or snippet: `RAN` (exit 0, no rollback by ABAPReach, as in SA38),
  `RUN_FAILED: exception <class>: <text>` (a catchable exception; rolled back),
  `RUN_FAILED: runtime error; see ST22 (dumps)`, `RUN_UNKNOWN: the reply was lost; the code may have
  run`, `SNIPPET_ERROR`. `--max-output <bytes>` (default 6 MiB; MCP 1 MiB) is counted by the runner
  in bytes of the reply and ends in `W: output cut after <n> lines` with the outcome `RAN`. The
  payload names the logon user and an expiry (generation time plus `--timeout` plus 20 minutes; RFC:
  75 minutes): a runner class left behind by a second Ctrl-C refuses to run again. The second
  Ctrl-C leaves with exit 130 and prints the `RUNNER_LEFTOVER` lines. The first Ctrl-C prints its
  waiting text (it was not printed before).
- Policy keys `run` (`off`, `plan`, `direct`; default `plan`), `mcp_run` (`off`, `confirm`; default
  `off`), `mcp_run_allow` (exact `TYPE:NAME` entries: `CLAS:`, `PROG:` or `FUGR:`; a plain name stops
  the configuration load; so does a name with a blank, a pattern character, an invalid character or
  a length over the limit of its type) and `mcp_run_snippet` (`off`, `on`; default `off`). `run:
  off` also refuses `run` and `aunit` on the CLI and in MCP. A 0.16 binary stops on a configuration that holds them.
- MCP tool `abap_run` (class, report and snippet), behind `mcp_run` and `mcp_run_allow` (a snippet also
  behind `mcp_run_snippet: on`); it and `abap_aunit` are
  listed only for a connection with `mcp_run: confirm` and `run` not `off`. It always asks the
  human in a form that shows the full content, whatever `mcp_write` says, refuses a confirmation
  longer than 6000 characters, takes the single write slot, and passes its output through the
  secret check. `read_table_denied` does not protect against what a run reads.

### Fixed
- `run PROG` of a report that sends a screen (dump `DYNPRO_SEND_IN_BACKGROUND`) is reported
  `RUN_FAILED: runtime error; see ST22 (dumps)` when the reply carries the English dump text, on
  ADT HTTP and on RFC; it was `RUN_UNKNOWN`. Seen live: on 750 both backends answer `Screen output
  without connection to user`; on 758 RFC answers the same and the ADT HTTP page says `Sending of
  dynpro <program> <screen> not possible: No window system type specified`. On 740 the RFC
  connection closes without a message, and the run stays `RUN_UNKNOWN`, as does any other HTTP 500
  and any other `RFC_CLOSED`.
- `delete FUGR` on SAP_BASIS 758: the member listing has an expandable `FUGR/FF` category node
  without a name, which stopped the delete with `member listing: invalid function module name ""`.
  The category node is skipped; an unnamed module node is still an error.
- `aunit` on SAP_BASIS 758: the existence read of the object sent `Accept: application/xml`,
  which the release answers with `HTTP 406`; it now accepts any representation.
- TABL conversion check on SAP_BASIS 758: the release has no function module `DD_DECIDE`, so the
  runner aborted and `deploy TABL` printed `ERROR` (`the simulate status is uncertain`). A change
  that needs the conversion verdict is now `REJECTED: The conversion check of <table> is not
  available on this release (no function module DD_DECIDE)`. Changes decided before that step
  (new tables, no change, table class) are unaffected.

## [0.16.1] - 2026-11-20

### Added
- `deploy`, `deploy --simulate` and `delete` over ADT: `--tr <request>` now adds a transportable
  object that no open request holds (the lock answers no `CORRNR`) to that request. Before any
  write ABAPReach reads the request from `E070` over the data preview and stops unless it exists,
  is a workbench request and not a task, is modifiable, and is owned by the logon user or holds a
  modifiable task of that user. `deploy` prints `transport request <request> (object added)`;
  `deploy --simulate` prints `WILL ADD <TYPE> <NAME> to <request>`. Without `--tr` the STOP stays
  and names the way out. The MCP `tr` argument follows the same rule.
- `doctor` prints `RUNNER out type: <interface>`, the type of the classrun `OUT` parameter, and a
  `RUNNER orphan: <class>` line for each runner class that has a `TADIR` row and no class. When
  either orphan preview returns its limit of 50 rows, `doctor` prints `RUNNER orphans: more than 50
  rows, list incomplete` instead of a list.

### Changed
- Classrun runner (issue #12): the runner class declares its output reference with the type of
  parameter `OUT` of `IF_OO_ADT_CLASSRUN~MAIN`, read from `SEOSUBCODF` before the class is created.
  `IF_OO_ADT_INTRNL_CLASSRUN` (7.40, 7.50; the generated class is unchanged) and
  `IF_OO_ADT_CLASSRUN_OUT` (newer releases) are known; any other type stops the run (exit 2) and
  names the type found. The new `SEOSUBCODF` read uses `IN` with one value per column, because a
  single-value `=` in a data preview `WHERE` is reported unreliable on some releases (issue #13).
- The source `PUT` of the runner class and of `deploy` (PROG, FUNC, CLAS, INTF and class includes)
  sends `Accept: text/plain`; a release that refuses a source `PUT` without it answered
  `HTTP 400: Accept header missing` (issue #12).
- Runner cleanup proof: after the class `GET` answers 404, a new session reads `TADIR` for the
  runner name. A row left there is a leftover (`RUNNER_LEFTOVER`); a failed read is
  `W: runner cleanup proof failed: TADIR <reason>` and also counts as a leftover. The same check
  runs when a lost create leaves the class state unknown and the class `GET` answers 404. When only
  the `TADIR` row is left, `RUNNER_LEFTOVER` adds `hint: only the object directory entry (TADIR)
  is left; remove it in SE03 (Change Object Directory Entries) or SE80` (issue #12).
- MCP server (issue #15): `connections.json` and `connections.local.json` are re-read on every tool
  call, so a connection added while the server runs resolves. Policies stay as the server started
  with them; a call that finds other policies logs `W: policies changed in connections.json; restart
  the MCP server to apply them` to stderr. A connection absent at start (new server or new client)
  gets the start top-level policies with `mcp_write` off and the built-in tables added to the deny
  list, until restart; a file that no longer parses stops the call with the parse error. A
  connection that the file named at start keeps its start target and credentials: when the file now
  resolves it differently, the call stops with `STOP: connection <ID> changed in connections.json
  since the MCP server started; restart the MCP server to use it` and nothing is contacted. See [MCP
  configuration](docs/mcp.md#configuration).

### Fixed
- `read` creates a missing parent directory of the output path (source, DDIC XML and `TRAN`
  output); a parent that cannot be created is `cannot create directory <dir>: <error>` (issue #14).
- Classrun runner (issue #12): the classrun `POST` now runs on a new session of the cycle backend
  (a new HTTP session, or a new RFC connection posting over SADT), opened after the source check
  and closed after the reply. The cycle session keeps the class lock, the delete and the proofs.
  On some releases (SAP_BASIS 758) a `POST` from the session that created and activated the class
  answered `Class does not implement if_oo_adt_classrun~main method!`, while a new connection ran
  the same class. A new session that cannot be opened is `REJECTED: runner POST failed: cannot
  open the POST session` before any `POST`, and the cleanup still runs.
- RFC backends (rfc-gui, rfc-sdk): a request sent over `SADT_REST_RFC_ENDPOINT` without an `Accept`
  header now carries `Accept: */*`; a request that names `Accept` is sent unchanged. On SAP_BASIS
  758 the endpoint answered `HTTP 400` to such requests, so `doctor` printed `RUNNER leftovers:
  unknown (search failed: HTTP 400)` (issue #12).

## [0.16.0] - 2026-11-12

### Added
- DDIC types `DTEL`, `DOMA`, `TTYP`, transparent tables and `INTTAB` structures (`TABL`), and
  transaction codes `TRAN`, for `read`, `deploy`, `deploy --simulate`, `create`, `create --simulate`
  and `delete`. `TRAN` has no `deploy`: use `create` or `delete`.
- Every operation on these types runs through one temporary runner class, over ADT classrun or RFC
  SADT. The class is named `<runner_prefix><8 characters>` and created in `$TMP` in each cycle, then
  removed. `runner_prefix` is set on a server entry of the config file; the default is
  `YCL_ABAPREACH_RUN_`.
- `deploy` of these types runs two cycles: run 1 shows the diff and the live hash, run 2 writes.
- Conversion verdict: a change that SAP converts with SE14 (for example a shorter field or a new
  type) is rejected with the SE14 reason. Deleting a table that holds rows is rejected.
- `doctor` prints `RUNNER` lines: the classrun availability, the runner prefix, the post transport,
  the preflight facts, and the leftover runner classes with their age and the commands that remove
  them.
- MCP: all seven tools that take a `type` accept `DTEL`, `DOMA`, `TABL`, `TTYP` and `TRAN`;
  `abap_deploy` and `abap_simulate` refuse `TRAN`. See
  [DDIC and `TRAN` types](docs/mcp.md#ddic-and-tran-types).

### Changed
- MCP: `abap_create` and `abap_create_simulate` refuse `prog` for every type except `TRAN`
  (`prog only applies to TRAN`), as the CLI does. 0.15.0 ignored it.
- An unpinned command on a stateless ADT session uses the RFC path. The CLI prints
  `NOTE: ADT HTTP is stateless on this release; the RFC path is used`.
- MCP: `abap_read_source` is no longer read-only (`readOnlyHint` is `false` for every type). A read
  of a DDIC or `TRAN` object takes the single write slot and creates the runner class, so a client
  that auto-allows `abap_read_source` also allows those classes. `abap_delete` always asks.
- DDIC `read`: a run that ends `NOT_FOUND` or `REJECTED` exits 1 and writes no file (Python exits
  2). See [DDIC `read`](docs/behavior-differences.md#ddic-read).
- System change option guard: the runner rejects only `EDTFLAG` `N`, SAP's standard rule. The Python
  tool rejected any non-empty value. See
  [System change option guard](docs/behavior-differences.md#system-change-option-guard).

### Fixed
- RFC backend: the connection now opens with `NO_BASXML=1` (classic RFC serialization). Before,
  `deploy` and `deploy --simulate` of a source of about 59 KB or more over RFC failed with
  `RfcInvoke: The read XML document had an invalid format` (an `XML_FORMAT_ERROR` dump in
  `SAPLSADT_REST`): the server could not read the basXML stream of a high-entropy `MESSAGE_BODY`
  from about 64 KB on.

### Known limitations
- A client killed (SIGKILL) while an ADT HTTP stateful session holds the class lock leaves that lock
  until the server session times out: about 38 minutes observed on a 7.50 system. `doctor` lists the
  runner class and the remove commands; they succeed once the lock is gone.
- Tested so far: the types and `TRAN` pass over ADT HTTP on the 7.50 system and over RFC SADT on the
  7.40 system. RFC SADT also works on the 7.50 system. ADT HTTP cells on the 7.40 system are
  UNSUPPORTED (no stateful session). The classrun POST also runs over RFC SADT, so a connection
  without an HTTP route can use these types.

## [0.15.0] - 2026-11-07

### Added
- `create PROG`: a program (`--subc 1`, the default) or an include (`--subc I`). The name is up to
  40 characters, starts with `Z` or `Y`, and the `--text` is up to 70 characters. `--package`
  defaults to `$TMP`. `create PROG --subc 1` takes an optional FILE and runs a simulate deploy
  after `CREATE: ACTIVE`; it writes no source. `--subc I` takes no FILE: deploy the source after
  the `INCLUDE` statement exists in the parent program.
- Program includes: `read`, `deploy`, `deploy --simulate` and `delete` of an include named as a
  `PROG`. `--main <PROG>` names the main program of an include. Without it, exactly one listed main
  program is used, which is printed as `main <PROG>`.
- MCP: `abap_create_simulate` and `abap_create` take `subc`; `abap_simulate`,
  `abap_deploy` and `abap_read_source` take `main`. `plan_token`s are bound to `subc`, `package`
  and `text`, and an include deploy runs under the main program that its simulate resolved.
- The simulate of a program delete lists the transactions that start the program as
  `WHERE-USED TRAN <tcode>`.

### Changed
- Every ADT HTTP stateful session ends with a logoff (`GET /sap/public/bc/icf/logoff`) on every exit
  path, in the CLI and in MCP. 0.14.0 ended the session with a discovery GET, which left enqueue
  locks in SM12. A failed logoff prints `W: session close failed: <reason>`; the status and exit
  code do not change, and the logoff is not retried.
- An ADT HTTP `delete` batch logs off the stateful session after each row it deleted, and the next
  row runs in a new session. A row that deletes nothing keeps its session.
- Include delete, and program and include where-used, run over RFC SADT as well, including on the
  SAP 7.40 system.
- Version metadata set to `0.15.0`.

### Known limitations
- TRAN is not supported in 0.15.0; it is planned for 0.16.0 over the ADT classrun path. See
  [TRAN is not supported](docs/behavior-differences.md#tran-is-not-supported).
- A new include's first deploy prints `W: no syntax check before the first activation of a new
  include (main <PROG>)`. The live matrix did not produce this state: activating a parent program
  that holds `INCLUDE <name>.` always fills the include's main program index. The warning path is
  covered by offline fixtures only.
- A transportable include delete over RFC SADT is proven by a fixture only. The 0.15.0 matrix
  records no live run of that case.
- OpenCode, pi and Codex were not rerun for 0.15.0. Their behaviour is unchanged from the 0.14.0
  gate and was not re-verified in this release.
- A SIGKILL during a write can still leave an ADT lock on the object. Check SM12 before a retry.

## [0.14.0] - 2026-11-04

### Added
- `read-table` command: reads an SAP table through the ADT data preview, with view resolution and
  routing between the ADT HTTP and RFC backends.
- `delete` command: planned delete of PROG, FUGR, FUNC, CLAS and INTF objects through a batch state machine, with ADT delete
  transport, admission checks and reconciliation of the outcome.
- MCP stdio server: JSON-RPC MCP protocol with read tools (untrusted payload and scrub rules) and
  write tools.
- `mcp_write` policy per connection (`off` by default, `confirm`, `token`) for the MCP write tools.
- `read_table_denied` deny list for `read-table`.

### Changed
- Version metadata set to `0.14.0`.

### Known limitations
- Codex is not verified for MCP in this release; the Codex gate was deferred.
- pi cannot answer an elicitation: writes under `mcp_write = confirm` are refused with a `STOP`
  that names the missing capability. Reads work.
- OpenCode has no Decline button; Esc is a cancel.
- `mcp_write` is read once at server start. An agent that can edit `connections.json` can relax
  it, and the change applies at the next start.
- A SIGKILL during a write can leave the ADT lock on the object. Check SM12 before a retry.
- A cancel after the source PUT ends as `PARTIAL` (source saved inactive, nothing activated);
  this case was not reproduced live with an MCP client.
- There is no restore after delete. `rfc-deploy restore` is not in this release.
- On a system whose ADT HTTP keeps no stateful session (observed on SAP_BASIS 7.40),
  `deploy --backend adt` stops before the lock, and a deploy or delete without the pin runs over
  RFC SADT.
- `read-table` MCP output is cut at 1 MiB and rows are capped at 1000 per call.

## [0.13.0] - 2026-11-03

### Added
- Source objects beyond `PROG`: `FUNC`, `CLAS` (main source and local includes) and `INTF` can be
  read and deployed.
- `FUNC` interface change is detected before deploy: the interface diff is printed and the callers
  found by where-used are listed as an advisory warning; the deploy is not blocked by it.
- Create `FUGR`, `FUNC`, `CLAS` and `INTF` objects, with name and text limits, `Z`/`Y` name rules,
  and a source-free skeleton. A create whose outcome is uncertain is reconciled, and no automatic
  rollback is performed.
- `read --lines` prints an inclusive line interval of the active source to stdout only. The full
  source is read and hashed before the projection.
- `--create-include` creates a missing local include of a `CLAS` in one POST. Without it, a missing
  include stops with `INCLUDE_MISSING: <kind> (re-run with --create-include)` and exit 1, with no
  write. This differs intentionally from the Python tool, which stops on a missing include.
- Transportable `CLAS` activation runs in two steps: when the first activation answers with an
  inactive-objects worklist, every worklist entry (including method entries) is sent again without
  pre-audit, only when every entry is under the class URI; an error message fails the deploy and a
  warning-only response is verified by rereading the source.
- RFC admission allow-list covers the new object kinds.
- Fault-injection builds accept `drop-reply:CREATE`, `fail-read:RECONCILE` and `hold-lock:<ms>`
  (1-60000; pauses after the LOCK until the time is up or Ctrl-C), combinable as a
  comma-separated `ABAPREACH_FAULT` list, to test uncertain create outcomes and interrupts.

### Fixed
- The inactive-sibling warning for function groups (`W: inactive objects in function group ...`)
  now works on SAP 7.40, whose inactive-objects feed is a flat `adtcore:objectReferences` list.
  RFC admission and the activation worklist keep reading only the 7.50 shape.
- Windows, SAP GUI 8.00 `sapnwrfccm.dll`: the library is a single-byte build, so the UTF-16 strings
  ABAPReach passed were read as one character (`RFC_INVALID_PARAMETER` from `RfcSetTraceDir` and
  `RfcOpenConnection`). The character width is now detected from the library and every string is
  passed in it; the RFC SADT probe passes with this library.
- Windows: `dev_rfc.log` no longer lands in the working directory. `RFC_TRACE_DIR` is also written
  to the environment of the shared UCRT (`ucrtbase.dll`), which is where the SAP libraries read it;
  this holds for the statically linked release build as well as for a dynamic-CRT build.
- Windows, NW RFC SDK: the ICU libraries beside a library named `sapnwrfc.dll` are mapped by
  absolute path before the library, so the SDK no longer exits at load with
  `Could not open the ICU common library` when its folder is not on `PATH`. A missing set prints a
  `WARNING: RFC ICU libraries were not found ...` line.
- `doctor` never prints the ADT host or the application server host: the `configured` lines show
  `***` for the host, and the text of a failed probe is scrubbed of both.
- `doctor` library line ends with `chars UTF-16` or `chars single-byte`, the character size of the
  loaded RFC library.
- A single-byte RFC library refuses non-ASCII request metadata (URI, headers, connection
  parameters) with a message that points to the NW RFC SDK library; ABAP source travels as XSTRING
  bytes and is not affected.

## [0.12.0] - 2026-11-02

### Added
- Release binaries for Windows x64 (`x86_64-pc-windows-msvc`), Linux x64 (`x86_64-unknown-linux-gnu`),
  Linux arm64 (`aarch64-unknown-linux-gnu`) and macOS arm64 (`aarch64-apple-darwin`), one archive per
  target with a `.sha256` file: `.zip` for Windows, `.tar.gz` for macOS and Linux.
- Per-OS RFC library discovery. The SAP GUI library (family `SAP GUI`) is tried first, highest
  version first; then the NW RFC SDK library (family `NW RFC SDK`). Lookup per OS is documented in
  `docs/getting-started.md` (RFC library lookup).
- `ABAPREACH_RFC_LIBRARY` sets an explicit library path. If that path cannot be loaded, ABAPReach
  stops and reports it; it never falls back to the discovered candidates.
- Architecture check before a library is loaded: a library built for another architecture is
  refused with `architecture mismatch: the library is <found> but this binary is <expected>`.
- Linux: ICU libraries beside the SDK library are preloaded; if they are missing, a
  `WARNING: RFC ICU libraries were not found beside the RFC library` line is printed.
- `doctor` prints the loaded library as `RFC library: <path> version <version> architecture
  <architecture> family <family> source <explicit ABAPREACH_RFC_LIBRARY or discovered>`, and the
  outcome of `RfcSetTraceDir` (exported and set, not exported and environment only, or returned rc).
- `doctor` reports the local credentials file permission: `local credentials: user-only permission`,
  or `local credentials: permission not checked` with a hint to restrict the file to the user.
- Deploy input accepts a UTF-8 BOM and CRLF line endings.
- TLS: HTTPS verification uses the operating system trust store. `ca_file` in an `adt` block adds
  PEM certificates on top of it; an unreadable file stops with `cannot read CA file <path>: <reason>`,
  and a file without certificates with `no certificate found in CA file <path>`.
- CI workflow `ci` and tag release workflow `release` (tags `v*`): native verification on four
  runners, packaged-binary checks, and release candidate promotion.

### Changed
- Ctrl-C, SIGTERM and SIGHUP are handled the same way. An interrupt while the lock is held unlocks
  first, then exits with code 130 (`INTERRUPT_EXIT_CODE`).
- The RFC trace environment is set before the signal handlers are registered, while the process
  still has one thread; `RfcSetTraceDir` then points the trace directory at the OS temp directory.

## [0.11.1] - 2026-11-01

### Added
- `<SID>_SAPROUTER` gives the SAProuter route of a system a home outside the shared config file.
  Lookup follows the credential keys (environment, then `connections.local.json`, then
  `servers[].rfc.saprouter` in `connections.json`); a route found there replaces the config value
  for every client of that system. A route may carry a `/W/<password>`, so it is never printed:
  `doctor` reports `SAProuter: configured (from <environment|connections.local.json|connections.json>)`
  and the route, its route passwords and router hosts are scrubbed from command output.
- For development only: the Cargo feature `fault-injection` (absent from release builds) and
  `ABAPREACH_FAULT=drop-reply:PUT|ACTIVATE`, which drops the reply of that RFC write after SAP
  executed it, to live-test reconciliation on RFC.

### Fixed
- A read-only GET that was sent but never answered counts as a connection failure: while no
  write was sent and neither `--base` nor `--backend` pins the backend, the command falls back
  to RFC. Before, `read` and `deploy` stopped with `post-send outcome uncertain`.
- `doctor` capability lines list only backends whose probe passed, `none` if neither.
- HTTP 401 on any ADT request, the CSRF token refetch included, ends with
  `ERROR: authorization failed: HTTP 401`, exit 1, no fallback and no retry, the same as a wrong
  password on RFC. Before, ADT HTTP stopped with `STOP: cannot read live source: HTTP 401`, exit 2.
- Lock warnings claim `(lock was released)` only after a successful UNLOCK reply; an unanswered
  LOCK or a failed session end after a lock warns `a lock on <NAME> may remain, check SM12`; a
  refused LOCK or a run without a LOCK no longer warns about a lock.
- An overwrite confirmed at the `ask` prompt says `(confirmed at the prompt)` instead of
  `(policy allow)`.
- The fallback note reads `failed before anything was written`.

## [0.11.0] - 2026-11-01

First release of the Rust implementation (milestone 1, partial scope).

### Added
- Single binary `abapreach` with `read PROG`, `deploy PROG` (`--simulate`, `--base`, `--tr`,
  `--backend adt|rfc`), `doctor` and `--version`.
- ADT HTTP backend: verified HTTPS by default, stateful session, CSRF token, cookies, no redirects.
- RFC backend: ADT requests tunnelled through `SADT_REST_RFC_ENDPOINT`, using the SAP GUI RFC
  library (or the NW RFC SDK) loaded at runtime; no SAP binaries are bundled.
- Capability routing: ADT by default; fallback to RFC only for failures before anything was sent
  or for a missing capability (for example an ADT server without stateful sessions); never with
  `--base` or `--backend`, never after an authorization error.
- Deploy safety: diff, base hash checked before and after the lock in the same session, syntax
  check without save, transport request rules, unlock on every exit including Ctrl-C.
- Reconciliation of uncertain writes by comparing active and inactive source content.
- Connection configuration compatible with the Python tooling (`connections.json`,
  `connections.local.json`, `<SID>_<CLIENT>_USER|PASS`), including RFC-only servers.
- `foreign_inactive` guard: before the lock, `deploy` and `deploy --simulate` read
  `GET <source>/versions` and refuse to overwrite the inactive version of another user
  (`REJECTED`, exit 1, nothing locked). The policy lives in the optional `policies` object of
  `connections.json` (`servers[].policies` overrides single keys): `reject` (default), `allow` or
  `ask`; an unreadable version list fails closed unless the policy is `allow`. `doctor` prints the
  effective policy.

### Fixed
- The `foreign_inactive` guard reads the source version list `GET <source>/versions`
  (`/sap/bc/adt/programs/programs/<name>/source/main/versions`, Accept
  `application/atom+xml;type=feed`). The program level `/versions` URI answered HTTP 404 on a
  live 7.50 and 7.40 system.
- The guard reads the version list a second time right after the lock, so an inactive version
  another user saves between the first check and the lock is still refused; the lock is released
  and nothing is written. A prompt is never shown while the lock is held: `ask` then behaves as
  `reject` for a version that appeared in between.
- The version list parser trims the `atom:id` and author values and reads CDATA sections, so a
  pretty-printed or CDATA-wrapped feed still names the inactive version and its owner.
- The `ask` prompt carries a one-line summary (`<NAME>: inactive version of <OWNER> would be
  overwritten (<n> lines changed vs active). Overwrite? [y/N] `) on stderr; stdout keeps the diff
  buffered until the run ends, so the answer was given without seeing the change.

**Earlier Python implementation (historical)**

## [0.10.0] - 2026-10-31
Python implementation: review fixes; all messages and documentation in English.

## [0.9.0] - 2026-10-31
Python implementation: transport gate, version restore, ENHO, DDLS, TRAN, INTF.

## [0.8.0] - 2026-10-31
Python implementation: message classes and text pools, short dumps, remote function test calls,
package export and drift, inactive objects and activation, ABAP Unit.

## [0.7.0] - 2026-10-31
Python implementation: DDIC objects (data elements, domains, tables, table types).

## [0.6.0] - 2026-10-29
Python implementation: planned, hash-gated delete.

## [0.5.0] - 2026-10-27
Python implementation: class metadata hardening and local class includes.

## [0.4.0] - 2026-10-26
Python implementation: `--base` hash, MCP server, sensitive-table denylist.

## [0.3.0] - 2026-10-25
Python implementation: object creation.

## [0.2.0] - 2026-10-25
Python implementation: server-side deploy function module and deploy client.

## [0.1.0] - 2026-10-24
Python implementation: RFC read client and shared ADT tooling.

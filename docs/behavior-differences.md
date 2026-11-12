# Behavior differences from the reference tooling

The Rust CLI is a port of an earlier internal tool. This page lists known
intentional behavior differences for operations available in the Rust CLI. It is not a feature-parity claim. See the [roadmap](roadmap.md)
for current scope.

Each entry names the live matrix scenario that exercises it, where one exists. An entry
marked "not exercised" describes implemented behavior that the release-candidate
matrix does not test.

The reference for the comparisons on this page is an earlier, unpublished internal tool. The
tool is not distributed with ABAPReach and no link to it is given. The table of differences
describes where ABAPReach behaves differently from that tool.

## Intentional differences

### 1. ADT delete transport selection happens at execute time

ADT simulation is read-only and cannot report which request currently owns a transportable
object. The execute-time LOCK reply decides the request; an explicit `--tr` must match it.
The LOCK reply also names the owner of the request (`CORRUSER`). When that owner is not the
connection user, ABAPReach releases the lock, sends no DELETE and reports `REJECTED` with the
request number; an owner the reply does not name (empty `CORRUSER`, for example on `$TMP`) is not
checked by ABAPReach. The 403 that SAP gives for a write with a wrong `corrNr` is a write-time
fact, not a LOCK reply, so it cannot be relied on to stop a foreign request before the DELETE.
Matrix: not exercised (requires a transportable delete fixture); covered by fixture tests.

### 2. Function modules use the ADT server-rendered format

The source of a `FUNC` is the ADT format: `FUNCTION <name> IMPORTING ... .` followed by the body.
The reference tooling uses the `*"` interface header in the file. A reference-format file is refused with
exit 2 (`STOP`). After a PUT, SAP re-renders the signature; ABAPReach compares the text of the PUT
reply, not the file bytes. Matrix: `func-python-file-rejected`, `func-deploy`, `bom-crlf-deploy`.

### 3. `CLAS` simulation does not save

`deploy CLAS --simulate` runs the syntax check through the checkrun and does not save. The matrix
checks that the active and inactive source are unchanged and that the output has one `live hash`
line. The reference function module writes the inactive version and then discards it. The transport
request entry is recorded as unobserved. Matrix: `clas-simulate-no-save`.

### 4. Source-free create

`create` makes an empty object (a skeleton) from the name, the text and the package or function
group. The reference `create` takes the file as an argument and writes the source with it. In
ABAPReach the source comes with `deploy`, after `create` and a simulation. Matrix: `create-*`
and `create-simulate`.

### 5. Uncertain create tokens

When a create POST outcome is unknown, ABAPReach reads the object and reports one of:

- `CREATE: PARTIAL` (exit 1): the object exists, or it cannot be read.
- `CREATE: REJECTED` (exit 1): the object does not exist; check the system before another run.

The reference tooling prints `CREATE status unknown` with no status token and does not read the object
back. Matrix: `create-uncertain-present`, `create-uncertain-unreadable`, `fault-create-drop` (RFC
paths). The `does not exist` branch is not exercised live.

### 6. Stricter name limits

ABAPReach checks the SAP limits per kind, before the create request:

| Kind | Name | Text |
| :--- | ---: | ---: |
| `PROG` | 40 | not applicable |
| `FUGR` | 26 | 40 |
| `FUNC` | 30 | 74 |
| `CLAS`, `INTF` | 30 | 60 |

Names for `create` must start with `Z` or `Y`. The reference tooling checks a single name limit of
40 for all kinds; its text limits are the same as above. Not exercised: the live matrix does not run the name or text limits.

### 7. `read --lines` writes to stdout only

`read ... --lines A-B` requires the output to be `-`. Any other value stops with exit 2
(`STOP: --lines requires OUT to be '-' (stdout)`). The reference `rfc-read source --lines` prints to
stdout with no output argument. The full source is read and hashed before the range is taken.
Matrix: `read-lines` and `read-lines-grammar`, both on `FUNC` only.

### 8. `--create-include`

The reference ADT tooling stops at an empty include. ABAPReach stops
with `INCLUDE_MISSING: <kind> (re-run with --create-include)`, exit 1, before any write. With
`--create-include` it sends one POST that creates the include, inside the class lock, and never
repeats it. The reference `rfc-deploy` has no local-include writer at all. Matrix: `include-missing`
and `include-create` (`CCAU`). The uncertain-POST branches are not exercised live.

### 9. Exit code 3 is unused

ABAPReach 0.13.0 never returns exit 3. Its exit codes are 0 (done), 1 (inspect the SAP state), 2
(`STOP`) and 130 (interrupt). The class metadata condition that would use exit 3
(`CLASS_METADATA_INCOMPLETE`) is not implemented, because the Class Builder check in the matrix
(`clas-verify4`) passed on the SAP 7.50 system.

### 10. Server-rendered FUNC compare and empty source

For a `FUNC`, the comparison is against the text SAP returns from the PUT, because SAP re-renders
the source (upper-case signature, blank lines). A fresh object's empty source is an empty body on
the SAP 7.50 system and a single NUL character on the SAP 7.40 system; both read as no lines, so
read and hash agree. A local file with no lines is refused (`STOP: source file is empty`), and no
empty PUT is sent. Matrix: `read-empty-canonical`, `func-deploy`, `bom-crlf-deploy`.

### 11. Where-used cap and package filter

A changed `FUNC` interface lists at most 50 callers, then `WHERE-USED ... <n> more`. Package entries
(`DEVC`) are not callers and are dropped; a caller that appears twice is listed once. Not exercised:
the 50-caller cap. The matrix (`func-signature-change`) checks on one caller that the list holds only
caller kinds and that no `more` line appears.

## `create PROG` grammar (0.15.0)

The 0.15.0 grammar covers the create and delete cases of the reference test suite
(`test_create_missing_flags`, `test_create_text_too_long`, `test_create_bad_flag_combinations`,
`test_create_file_problems`). The intentional differences:

- `create PROG <NAME> --text T` needs no `--package`: ABAPReach defaults to `$TMP`, as it already
does for the M3 kinds (reference stops: `create PROG needs --package`).
- `create` takes no FILE for the M3 kinds (source-free create, difference 4);
`create PROG --subc I` refuses FILE with the reference message about deploying the source after the
`INCLUDE` statement exists in the parent program; `create PROG --subc 1 FILE` accepts the file and
runs a simulate deploy after `CREATE: ACTIVE`, never a source write (reference writes the source with
create). Matrix: `create-prog-file`.
- The create forms that the Rust grammar does not know are refused by the CLI parser instead of
by validation: `--remote`, `--base` and `--force` on any create. For the DDIC kinds, `--subc`,
`--fugr` and `--remote` stop with exit 2 before any connection (see [DDIC and TRAN](#ddic-and-tran-differences-0160)).

## DDIC and TRAN differences (0.16.0)

Data elements, domains, tables, table types and transaction codes run through a temporary class
(classrun). The lettered entries are the intentional differences from the reference behavior that
the 0.16.0 contract settled. Operations and their footprint are in
[Deploy safety: DDIC and TRAN](deploy-safety.md#ddic-and-tran).

- **a. Temporary class.** Read, simulate, deploy, create and delete each create, run and delete a
  temporary class in `$TMP`. Every such result carries a `RUNNER:` line. The reference reads through one
  function module and touches no repository object.
- **b. Two runs for deploy.** Run 1 prints the diff and the live hash before any write; run 2 writes
  with `--base`. The reference makes one function-module call.
- **c. Where-used token.** A `WHERE-USED` line prints `(transparent)` for a transparent table. The
  reference prints a localized token.
- **d. `--text` that differs from the XML.** A DDIC `create` whose `--text` differs from the description
  in the XML stops with exit 2 before any connection. The reference ignores the text.
- **e. Preflight.** A `read` or simulate of a DDIC or `TRAN` object stops with exit 2 on a system that
  refuses repository changes (see [Deploy safety: DDIC and TRAN](deploy-safety.md#ddic-and-tran)). The
  reference `read` runs there.
- **f. Leftover runner class.** When the temporary class cannot be removed, the result carries the
  token `RUNNER_LEFTOVER` and exits 1. The text names the class and the two commands that remove it.
- **g. Access paths.** ADT HTTP is `UNSUPPORTED` on the 7.40 system for these types: the temporary class
  needs a stateful session, and that system keeps none (`ADT HTTP session is stateless on this release;
  the temporary runner class needs a stateful session (use the RFC path)`). The RFC backend posts the
  classrun over `SADT_REST_RFC_ENDPOINT` on both systems.
- **h. Limits and subsets.**
  - The file size limit is 256 KiB.
  - `TABL` writes cover transparent tables (`TRANSP`) and structures (`INTTAB`) only. Other table classes
    can be read; a write or delete is `REJECTED` with `TABCLASS <x> not supported`.
  - `TRAN` has no `PARITY` line.
  - `read` of a DDIC or `TRAN` object is refused under `mcp_write = off` (see [MCP server](mcp.md#ddic-and-tran-types)).
  - `readOnlyHint` of `abap_read_source` is `false`, because it creates a temporary class for these types.

Entries that are not in the list above, decided by the maintainers:

- **i.** The XML travels in the payload of the temporary class. ABAPReach passes it as payload, not as a function module parameter.
- **ii.** Warnings (`W:`) go to stderr, as in every command since 0.11.0. Stdout carries only the result.
- **iii.** A `read` whose live object is not active, or whose hash does not match, exits 1 (the operation
  failed after a connection). The reference exits 2, which is a stop before anything ran.

The conversion verdict follows the SAP activation rules: a change that needs SE14 conversion is
refused. A table that holds rows is not converted or deleted (`Table <name> (N rows) needs SE14 conversion;
change rejected`). On an empty table the conversion is allowed.

### TRAN (0.16.0)

A transaction code is created, read and deleted; it is not deployed. `deploy` refuses it with
`TRAN has no deploy; use create or delete`.

- `create` takes `--prog <REPORT>`: an existing executable `Z` or `Y` report program. The name is at most 20
  characters.
- `read` writes six lines: `TRAN <tcode>`, `program=`, `dynpro=`, `cinfo=`, `text=` and `package=`, and their hash.
- `read` and `delete` do not check the transaction type. The delete plan prints `cinfo`. It has no
  `WHERE-USED` line, so the `WHERE-USED` token of the reference plan is not compared.
- The reference tool has `TRAN` methods but no test cases for them. The `TRAN` coverage is ABAPReach's own.

### Access path comparison (0.16.0-rc.6)

The three paths were compared on the 0.16.0-rc.6 binary, 2026-11-12. The paths are:

- classrun: ABAPReach's path.
- function module: a custom function module, not part of ABAPReach, that must be installed on the SAP system.
  It exists on the 7.50 system only.
- ADT built-in: the endpoints ADT provides for the type.

Each cell ran three times; the timings are medians. The results on the 7.50 system:

| Type | Operation | Classrun | Function module | ADT built-in |
| :--- | :--- | :--- | :--- | :--- |
| `DTEL` | read | PASS | PASS | PASS |
| `DTEL` | simulate | PASS | PASS | UNSUPPORTED: no simulate for DDIC objects |
| `DTEL` | deploy | PASS | FAIL | UNSUPPORTED: write returns HTTP 400 (search help element expected) |
| `DTEL` | create | PASS | FAIL | UNSUPPORTED: create returns HTTP 400 (search help element expected) |
| `DTEL` | delete | PASS | PASS | PASS |
| `DOMA` | read, simulate, deploy, create, delete | PASS | PASS | UNSUPPORTED: no ADT endpoint on 7.50 or 7.40 |
| `STRU` | read | PASS | PASS | PASS |
| `STRU` | simulate | PASS | PASS | UNSUPPORTED: only read is implemented |
| `STRU` | deploy | PASS | FAIL | UNSUPPORTED: only read is implemented |
| `STRU` | create | PASS | PASS | UNSUPPORTED: only read is implemented |
| `STRU` | delete | PASS | PASS | UNSUPPORTED: only read is implemented |
| `TABL` | read, simulate | PASS | PASS | UNSUPPORTED: no ADT endpoint on 7.50 or 7.40 |
| `TABL` | deploy | PASS | FAIL | UNSUPPORTED: no ADT endpoint on 7.50 or 7.40 |
| `TABL` | create | PASS | FAIL | UNSUPPORTED: no ADT endpoint on 7.50 or 7.40 |
| `TABL` | delete | PASS | PASS | UNSUPPORTED: no ADT endpoint on 7.50 or 7.40 |
| `TABL` | conversion guard (SE14, table with rows) | refused | refused | n/a |
| `TTYP` | read, simulate, deploy, create, delete | PASS | PASS | UNSUPPORTED: no ADT endpoint on 7.50 or 7.40 |
| `TRAN` | read, simulate, create, delete | PASS | PASS | UNSUPPORTED: no ADT endpoint on 7.50 or 7.40 |
| `TRAN` | deploy | not applicable | not applicable | not applicable |

On the 7.40 system only classrun runs: every applicable cell passes on the RFC path. The ADT built-in
endpoints do not exist there, and the function module is not installed.

Verdict: classrun stays. The function module path is about 12 times faster on the 7.50 system (summed
median times 16.6 s against 205 s over the compared cells), but it fails five cells on correctness:
data element deploy and create, structure deploy, and table deploy and create. Its activation check
reports that the active version does not match the file for files that are not in SAP's canonical form.
In the first run classrun had one cell that did not match; after the table rerun it has no FAIL cell.
ADT built-in endpoints cover only data element read and delete and structure read on the 7.50 system,
and nothing on the 7.40 system.

## `create PROG` operation (0.15.0)

The create operation covers the `create PROG` cases of the reference test suite
(`test_create_no_file_active`, `test_create_with_file_prog_and_clas`, `test_create_rejected`,
`test_create_exception_unknown_status`, `test_create_failure_after_active_exit_1`,
`test_create_read_failure_after_active_exit_1`, `test_create_simulate_prog_exact_kwargs`).
The intentional differences:

- An existing name, as a program or as an include, is `CREATE: REJECTED` (exit 1) from a read of
`programs/programs` and `programs/includes` before any create request; both must answer 404.
`create --simulate PROG` is that read plus the local checks and the package and `--tr` rules; the
reference `CREATE_SIM` call to the function module has no ADT counterpart.
- `programs/validation` is called and shown as a `W: validation:` line when it objects, and never
decides anything: SAP answers `CHECK_RESULT X` even for a name outside `Z`/`Y`. The local `Z`/`Y`
and length checks are the guard.
- An include is created by one POST to `programs/includes` without a `containerRef`; its main
program is bound by the first deploy (`--main`). A transportable package always sends `corrNr`, and
`--tr` is required there (SAP otherwise generates a new request); the check is local, before any
request, with exit 2.
- A failed or lost create reply is settled by a read of the same properties: the object exists
(`CREATE: PARTIAL`), it does not (`CREATE: REJECTED`), the read fails (`CREATE: PARTIAL`); the
create is never repeated. The reference text `CREATE status unknown ...` (no status token) is
replaced by these tokens.
- With FILE, the simulate deploy after `CREATE: ACTIVE` runs on the same session and writes no
source. When it does not end well, the result is `CREATE: PARTIAL` (exit 1) followed by the deploy
output, replacing the reference text `object PROG <NAME> was created; source not yet deployed`.
reference also stops with exit 1 when the read before its simulate fails; the simulate deploy of
ABAPReach covers that read.
- Over ADT HTTP an include created on a 7.40 system may not be visible right after the create (the
HTTP session is stateless there); creates on that system run over the RFC path.

## Program includes and ADT HTTP logoff (0.15.0)

These differences were settled from the 0.15.0 matrix. Each entry names the
evidence it rests on; where the matrix did not produce a case live, the entry says so.

- **Include resolution.** A `PROG` name is read through `programs/programs/<name>` first. Only a
404 on `<program>/source/main` makes ABAPReach try the include URI `programs/includes/<name>`; the
program path keeps the request order of 0.14.0. When both answer 404, deploy prints
`NOT_FOUND: PROG <N> (no program or include of that name)` (exit 1), and read prints
`read PROG <N>: NOT_FOUND (no program or include of that name)`.
- **`--main`.** A read of an include needs no `--main`. `--main` with a program is a STOP:
`--main is only valid for an include; PROG <N> is a program` (exit 2). Deploy and simulate of an
include print `main <PROG>` with the program that was used.
- **Choosing the main program.** `--main` must be one of the programs that
`GET programs/includes/<n>/mainprograms` lists, when any are listed; otherwise it is a STOP with
`main program <P> is not a main program of include <I> (listed: ...)`. `--main` must also exist as a
program and its active source must hold the statement `INCLUDE <I>.`; the STOPs are
`main program <P> does not exist as a program` and
`main program <P> does not contain the statement INCLUDE <I>.` (exit 2). When none is listed,
`--main` supplies the program. Without `--main`, exactly one listed program is used; none listed
gives `include <I> has no main program; pass --main <PROG>`, and several give
`include <I> has several main programs (<list>); pass --main <PROG>`. Both are exit 2. A
`mainprograms` entry that is not `programs/programs/<name>` (for example a function group) is a
STOP.
- **Not-processed check.** A checkrun that answers `notProcessed` is never `SIMULATED_OK`:
`REJECTED: the check was not processed: <statusText>` (exit 1). The one exception is a new include
whose `mainprograms` is empty and whose `--main` is given: see the next entry.
- **New include.** The first simulate of a new include, with an empty `mainprograms` and `--main`,
prints `W: no syntax check before the first activation of a new include (main <PROG>)`, and the
deploy continues with the contextual activation as the check. The decision comes from the empty
`mainprograms`, never from SAP's `statusText`. On the
matrix systems this state was not produced live: activating a parent that holds `INCLUDE <name>.`
always fills the index, so the warning path is covered by offline fixtures only.
- **Activation worklist of an include.** Any worklist answer to an include activation ends
`INACTIVE` (exit 1) and is not sent again. The reference tooling may behave differently; this was not
compared.
- **Include create.** One `create PROG <NAME> --subc I` POST creates the include, without a
`containerRef`. Its source is a separate `deploy PROG <NAME> <FILE> --main <PROG>`; the parent
program that holds the `INCLUDE <name>.` statement is deployed on its own.
- **ADT HTTP logoff.** Every ADT HTTP stateful session ends with `GET /sap/public/bc/icf/logoff`,
on every exit path, one per session (each independent read session included). 0.14.0 ended with a
discovery GET and left enqueue locks in SM12. A failed logoff prints
`W: session close failed: <reason>`, is not retried, and does not change the status or exit code.
A call that never opened a session sends no logoff. RFC SADT is unchanged: its session ends with
the connection. SIGKILL cannot be caught and can still leave locks. The process-level signal test
runs on unix only; Windows console events (Ctrl-C) were not exercised live: the 0.15.0 matrix
has no interrupt cell. It checks that no enqueue lock remains after every ADT HTTP cell.
- **Delete batch over ADT HTTP.** After each row whose DELETE answered 2xx, the stateful session is
logged off and the next row runs in a new session. A row that deletes nothing keeps its session. RFC
SADT is unchanged. Within a batch, programs are deleted before their includes.
- **Include delete and where-used.** The program and include where-used of the delete plan runs
over RFC SADT as well, on both systems. SAP refuses to delete an include that a program still
references (HTTP 403, `Program <N> is referenced in other programs`).
- **Transportable packages.** Every POST, PUT and DELETE on an object in a transportable package
carries `corrNr`. A create without `corrNr` makes SAP generate a new request, so a create in a
non-`$TMP` package without `--tr` is refused before any request (exit 2).

## System change option guard

The system change option guard of the runner class (`TRAN` and DDIC write modes) rejects only
`EDTFLAG` `N`, the SAP standard rule in `TR_SYS_GET_EU_LOCK_STATUS`. The reference tool rejected any
non-empty value.

## DDIC proof after activation

The in-run comparison of the file with the active definition does not count the master language
(`DTELMASTER`, `DOMMASTER`, `MASTERLANG`) as a difference when the file leaves it empty: SAP fills
it with the original language. A value in the file must still match. A failed comparison names the
first differing component with the file value and the active value.

## DDIC `read`

`read DTEL|DOMA|TABL|TTYP <NAME> <OUT>` runs one temporary class in `$TMP` (the repository
preflight first, the class removed at the end) and writes the XML of the active definition. The
reference tool calls one function module and touches no repository object. Further differences:

- **Status exits.** A run that ends `NOT_FOUND` or `REJECTED` is exit 1 and writes no file. reference
  stops with exit 2. A client hash that differs from the hash the run reported is exit 1 with no
  file (reference: exit 2).
- **Confirmation from a new session.** The definition must have a row in the definition table and in
  `TADIR` when a new session reads them; a definite difference is `PARTIAL` (exit 1, no file). A
  preview that cannot be read is a `W:` note and does not stop the read.
- **Uncertain reply.** A lost, cut, or broken reply is exit 1 with no file and no reconciliation
  (a read changes nothing in SAP); the read is not resent.
- **`--lines`.** reference has no such option; here it is refused for these types (exit 2).
- **Control characters.** A row of the XML with a control character other than a tab is refused,
  because the write side refuses it, so such a file could not be deployed again.
- **A failed XML transformation** (for example `TABL TSTCT` on one system) is `REJECTED` with the
  error text. It was reported as `NOT_FOUND` before.

## Observed parity notes

Source code (0.13.0): these results come from a run that deployed the same change with the
reference tooling and with ABAPReach to twin objects on the SAP 7.50 system. The final run gave
30 PASS, 6 EXPECTED and no FAIL. It did not compare on the SAP 7.40 system, because the reference
deploy function module is not installed there.

DDIC types and `TRAN` (0.16.0-rc.6): the same comparison on the 7.50 system gave 6 PASS,
24 EXPECTED (each an intended difference listed on this page), no FAIL and 3 UNSUPPORTED (a
restricted-authorization preflight, the cleanup-failure token that needs a fault-injection build,
and the `PARITY` line the reference prints only for function modules). The 7.40 system has no
reference side for the same reason as above.

- **Local includes.** The reference `rfc-deploy` has no local-include writer, so the `CLAS` with
  `CCIMP` row has no reference side (EXPECTED).
- **Change after the final `ENDCLASS`.** The reference tooling refuses a change appended after the
  final `ENDCLASS` of a class (`Active source content ... changed when inactive was discarded`).
  ABAPReach deploys it and reports `ACTIVE` only after the active class source it reads back equals
  the file. A reread with either tool afterwards shows the appended text in the active source.

## Transport requests, inactive versions and related behavior

### Inactive version after a failed activation

The reference tooling discards the inactive
version when activation fails. The ADT paths of `abapreach` (HTTP and
RFC/SADT) do not: after a failed activation, the failed source remains as
the program's inactive version (an entry in `DWINACTIV`), the active source
is unchanged, and the status is `INACTIVE` with exit 1. Inspect the inactive
version before attempting another deploy.

### A transportable object whose lock reports no transport request is refused

When the lock of a transportable object (package not `$TMP`) answers 200
without a `CORRNR`, `abapreach` stops with `STOP: the object is not local but
the lock returned no transport request` (exit 2) after releasing the lock
and before the syntax check and PUT. This holds even when `--tr` is given:
an empty `CORRNR` is never replaced by `--tr`.

The reference tooling would still write there. For a transportable object it
requires `--tr`, checks that the request is a modifiable workbench request
owned by the user (`E070`) and that the object is not enqueued in another
request (`TLOCK`), then passes `--tr` as `IV_TRKORR`; the function module
locks with exactly that request (`set_lifecycle_corr`: `mo_lm->corrnr =
mv_trkorr`). The request of the write is therefore chosen by the client,
not read from the object lock.

`abapreach` stays strict on purpose: on the ADT path, the `CORRNR` of the
lock response is the only statement SAP makes about the request that owns
the object lock. With an empty `CORRNR`, the tool cannot prove that the
`--tr` request owns the object. It refuses before writing and releases the
lock. Assign the object to a transport request on the SAP side, then retry
only after verifying the lock response.

### Related behavior and comparison limits

- With the default `foreign_inactive: reject` policy, `abapreach` refuses
  another user's inactive version, matching the reference function module's
  verdict. The reference tooling always refuses; the `allow` and `ask`
  policies exist only in `abapreach`.
- `deploy --simulate` writes nothing in both tools (a syntax check only);
  the active hash, inactive entries, and enqueue table are equal afterward.
  For `CLAS`, see difference 2.
- Both Rust backends compute `--base` from the normalized active source, the
  same way as the reference tooling. A live comparison on SAP_BASIS 7.50 gave
  the same source and the same hash from Rust `read` and from the reference
  function module, so a hash from either tool is accepted by the other.
- `GET <source>?version=inactive` without an inactive version can answer
  200 with the active source, so reconciliation compares content rather than
  relying on that HTTP status alone.

### `read-table` against the reference `rfc-read table`

- `--rows 0` is refused (STOP, exit 2); the reference tool reads every row for
  `rows=0`. The ADT data preview has no "all rows" mode, so `--rows` is 1 to
  5000, the largest value the 7.40 data preview honours (MCP: 1 to 1000).
- The backend is the ADT data preview, on ADT HTTP and over RFC. The reference
  tool calls `RFC_READ_TABLE`; its 72-character `split_where` is ported
  (`split_where`, literal-safe) but not on the default path.
- `--where` accepts only the tokenized grammar: `field op literal` with
  `= <> < > <= >= LIKE IN BETWEEN IS [NOT] NULL`, `AND`/`OR`/`NOT` and
  parentheses. The reference tool sends the text through unchanged. Fields,
  second tables, `~`, `@`, comments, subqueries and joins are refused before
  any connection, because SAP runs a data preview body as written.
- A view is read only when it is a database view and none of its base tables
  (`DD26S`) is on the `read_table_denied` list. A name missing from `DD02L`,
  a failed dictionary lookup, a CDS/DDLS entity or another view type is
  `REJECTED` (exit 1) and nothing is read.
- Each command uses a fresh connection; the data preview over RFC fails on
  the 37th call of one connection.

## `delete` (shared operation, 0.14.0)

The Rust delete operation covers the cases of the reference test suite that apply to
the ADT route (`tests/delete_operation_test.rs`). Intentional differences:

- The plan hash also covers the active source of every source object (`CLAS` main
  source plus each local include, `FUNC`, `INTF`, `PROG`), or the marker `NO_ACTIVE`. The
  reference plan hash covers only the plan lines. Where-used is reported and never hashed.
- A real delete reads each object again and compares its plan hash itself, because there
  is no function module to do it. A changed object is `REJECTED` with "Plan changed since
  simulate; simulate again", and the batch continues.
- `foreign_inactive` applies to delete (`reject` by default; `ask` prompts on a terminal
  and counts as `reject` under MCP). reference has no such guard for delete.
- Not ported because there is no function-module call on the ADT route: the batch list
  (`IT_SOURCE`) sent to the function module, and the "empty plan hash" case (a hash is
  always computed locally).
- Not ported here because the CLI grammar is not part of this operation: duplicate-object
  and `--tr` syntax are checked by the operation, option combinations by `clap`.
- A lost delete reply is reconciled once from a new session and never retried
  (`DELETED (reconciled)` exits 1); reference reports an unknown status and stops.
- `delete --simulate` prints the guards that apply and the function-module guards that
  do not apply on the ADT route (system change option, `CCNOCLIIND`, SE06 and software
  component).
- Backend fallback: when ADT HTTP fails before sending (connect, DNS, TLS) while the batch only reads, the whole batch runs again on RFC SADT and prints the usual `falling back to RFC SADT` note. This holds until the first LOCK or DELETE request is started; after that, and after a post-send, authorization, conflict or validation failure, there is no fallback. A Ctrl-C or an MCP time limit that passed ends the call instead of opening RFC. The CLI plan file records no backend (as in reference), so `delete --plan` may therefore execute on a different backend than the one that simulated, before any write; the execute re-reads and re-hashes every object, so a difference between the two routes is a `REJECTED` object, never a wrong delete. An MCP `plan_token` pins the backend it was issued for: its execute never falls back and reports that the `plan_token` pins the backend.

## MCP server and `read-table` (0.14.0)

- The reference tooling has an MCP server. `abapreach mcp` ports its tools and its plan-token gate
  to the Rust CLI; see [MCP server](mcp.md).
- The MCP write gate is stricter than the CLI: a `plan_token` is single-use and lives 15 minutes,
  `mcp_write` defaults to `off`, and a delete always needs a human confirmation through
  elicitation. The CLI has no token.
- `read-table` rows: CLI 1 to 5000, MCP 1 to 1000 (default 100). A value outside the range is a
  `STOP` with exit 2 on the CLI.
- `delete`: the reference tooling deletes through `rfc-deploy`. The Rust delete runs ADT HTTP
  first and falls back to RFC SADT only before the first LOCK; the plan file records no backend.
  See [Deploy safety: Delete](deploy-safety.md#delete).
- Parity with the reference tooling: delete parity (batch and `INTF`) passes on every path on a 7.50
  system; on a 7.40 system without the reference deploy function module the reference side cannot run, so
  delete parity is not tested there. `read-table` parity passes on both.

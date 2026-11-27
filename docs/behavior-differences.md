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

### 12. `where-used` goes through ADT first (0.18.0)

The reference `where-used` reads the standard cross-reference tables (`WBCROSSGT`, `WBCROSSI`) through
the internal data preview for every type. ABAPReach asks the ADT `usageReferences` search first and
tags each line `via=ADT`. Only for a DDIC type whose collection the release does not list, or that
answers HTTP 404 or 500, does it read the cross-reference tables (`WBCROSSGT`, plus `DD03L` for a data
element and `DD04L` for a domain), tags the line with the table (`via=<table>`) and prints a `W:` line
on stderr. A `TABL` takes the `tables` or the `structures` collection by its table class. The line
format is the reference one (`USED-BY <TYPE> <NAME> package=<P>`, sorted, each once, then
`WHERE-USED: <n>`) with the added `via=` field. It lists objects only, not lines of code. `MSAG`
(planned for 0.19.0) and `TRAN` stop, and a reply over 64 MiB is refused. `delete --simulate` keeps its own where-used (difference 11).
Matrix: `m8-where-used-func`, `m8-where-used-dtel` and `m8-where-used-struct` ran on `v0.18.0-rc.3` on the
7.50 and 7.40 systems (macOS arm64, Linux x64, Linux arm64, Windows x64 native, Windows x64 emulated on
arm64) and on the 758 system (macOS arm64, Linux x64), on ADT HTTP, RFC through the SAP GUI library and RFC
through the NW RFC SDK: 97 PASS, 0 FAIL, 11 UNSUPPORTED. The cells assert the exit code, the `via=` tag and
the fallback line, that the generated report is listed as a caller, and that the test objects are gone.
UNSUPPORTED: `m8-where-used-dtel` over ADT HTTP on the 7.40 system (5 cells, one per host), because the case
creates a temporary runner class and ADT HTTP is stateless there; and RFC through the SAP GUI library on
Linux arm64 (6 cells, 3 scenarios on each of the 7.50 and 7.40 systems), because SAP GUI for Java for Linux is
x86_64 only.

### 13. `dumps` reads `FLIST` to `FLIST08` and lists one user by default (0.18.0)

The reference reads the dump text from `FLIST` to `FLIST05` of table `SNAP`. ABAPReach reads
`FLIST` to `FLIST08`, so a dump whose tag stream is longer than `FLIST05` is complete. `dump` prints only the decoded tags (error, program, include, line), the user and
client, the short text from `SNAPT` and a source excerpt, never the raw stream. The reference lists
the dumps of every user. `dumps` lists the logon user's dumps unless `--all` or `--user` is given,
and `dump` of another user's dump needs `--all`. `--all` is a display filter: `SNAP` is not on the
`read_table_denied` list, so `read-table` and `sql` can read it where the data preview allows. On the
7.50 system the data preview refuses `SNAP`; the RFC paths then read it through `RFC_READ_TABLE`
(needs an English logon), and ADT HTTP stops with the hint `--backend rfc`. Matrix: `m8-dumps-list` and
`m8-dump-show` ran on `v0.18.0-rc.3` on the same systems, hosts and paths as the where-used cells: 57 PASS,
1 FAIL, 14 UNSUPPORTED. The cells raise a dump with a temporary runner class and assert the list format
(newest first, the logon user's entries, the row cap), the decoded fields of `dump`, that another user's
dump stops and that an unknown id is not found. On the 7.50 system the ADT HTTP cells (10 PASS) assert the
stop with the `--backend rfc` hint and the fallback of an unpinned backend. The RFC cells pass on the
7.50 system (8 on the SAP GUI library, 9 on the NW RFC SDK), on the 7.40 system (8 and 10) and on the 758
system (all three paths, 4 cells each). The one FAIL is `m8-dump-show` on the 7.50 system, RFC SDK, Windows
x64 native: the cell was rerun alone, without `m8-dumps-list` in the same run, and stopped with
`dump-id-missing`; the full run of the same cell passed. UNSUPPORTED: ADT HTTP on the 7.40 system (10
cells), where the dump-raising runner class needs a stateful session, and RFC through the SAP GUI library
on Linux arm64 (4 cells).

### 14. `sql` is a closed grammar (0.18.0)

There is no reference command: the earlier tooling reads one table at a time. The ADT freestyle data
preview runs open SQL as written, so ABAPReach does not pass your text on. It accepts one `SELECT`
(joins, `WHERE`, `GROUP BY`, `HAVING`, `ORDER BY`, five aggregates), parses it, and sends only its
canonical single-line rendering; the freestyle request is refused at the transport boundary
unless it equals the canonical rendering of its own parse. Subqueries, `UNION`, `EXISTS`, `WITH`,
`INTO`, `UP TO`, host variables, path expressions, comments, `RIGHT` and `CROSS` joins and the client
options are refused with `STOP` (exit 2). Every entity in `FROM` and `JOIN` passes the
`read_table_denied` list and the dictionary check of difference 15. Matrix: `m8-sql-join-sap`,
`m8-sql-join-ytest`, `m8-sql-denied-join` and `m8-sql-cds-view` ran on `v0.18.0-rc.3` on the 7.50, 7.40 and
758 systems (hosts as for difference 12) on all three paths, so ADT HTTP, RFC through the SAP GUI library
and RFC through the NW RFC SDK each send the canonical rendering: 136 PASS, 0 FAIL, 8 UNSUPPORTED. The cells
assert a join of standard tables with the header, the rows and the row cap, a join that reads a
test row created for the cell, the `STOP` for a join with a denied table and the `STOP` for a
CDS entity. UNSUPPORTED: RFC through the SAP GUI library on Linux arm64 (8 cells, 4 scenarios on each of the
7.50 and 7.40 systems), because SAP GUI for Java for Linux is x86_64 only.

### 15. `read-table` fails closed on dictionary refusals (0.18.0)

Before 0.18.0 `read-table` checked the deny list against the named table only, so a DDIC view over a
denied table passed. Now the named entity goes through the dictionary: a table of class `TRANSP`,
`CLUSTER` or `POOL` is read; a DDIC database view is read only when `DD26S` lists its base tables
and each one passes in turn; a CDS entity, a generated CDS SQL view (`DDLDEPENDENCY`), a view without
base tables, a structure and an unknown name are refused. A refusal is `STOP` with exit 2; it was
`REJECTED` with exit 1. A dictionary lookup that fails stays `REJECTED` with exit 1. The check costs up
to 24 dictionary lookups, so a view with more than about 21 base tables is refused. This is a
behavior change from 0.17.x. Matrix: `read-table-view-denied`, `read-table-unknown`,
`read-table-denied-case` and `read-table-denied-default` ran on `v0.18.0-rc.3` on the 7.50, 7.40 and 758
systems (hosts as for difference 12) on all three paths: 116 PASS, 0 FAIL, 28 UNSUPPORTED. The cells assert
the `STOP`, the exit code and that no rows are returned. The view cell (`read-table-view-denied`) ran on
the 7.40 system only (14 PASS: five hosts, three paths, with the view `SBC_CLIENT` over the base table
`T000`); on the 7.50 and 758 systems it is UNSUPPORTED (20 cells) because no database view over a harmless
base table is known there. The other UNSUPPORTED cells (8) are RFC through the SAP GUI library on Linux
arm64.

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
- **Simulate `CONVERSION` line.** The line carries one more field, `daction=`, the database action from the
  `DD_DECIDE` result for the table; an `MDF` whose `daction` is `ALT` is treated as `ALT`. Example:
  `CONVERSION <table>: action=ALT modeflag=F daction=ALT allowed=X rows=0`. The reference line has no
  `daction=`; the other fields are the same.

The conversion verdict follows the SAP activation rules (`DD_DECIDE`): a change SAP applies
directly, such as a new field, is activated; a change that needs an SE14 conversion is refused before
anything is written (`Table <name> (N rows) needs SE14 conversion; change rejected`). This holds on an
empty table too: removing a field from a table with 0 rows was refused in the rc.6 path comparison.
A table that holds rows is not deleted.

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

Verdict: classrun stays. The function module path needs a custom function module installed on the SAP
system, which ABAPReach does not require anywhere. It is about 12 times faster on the 7.50 system (summed
median times 16.6 s against 205 s over the compared cells), but it fails five cells on correctness:
data element deploy and create, structure deploy, and table deploy and create. Those failures come from its strict
activation check, which reports that the active version does not match the file for files that are not
in SAP's canonical form.
In the first run classrun had one cell that did not match; after the table rerun it has no FAIL cell.
ABAPReach does not use the ADT DDIC endpoints. In this comparison, data element read and delete and
structure read worked over them on the 7.50 system; a data element create or deploy failed with HTTP 400
on the request the comparison sent (a missing `searchHelp` element), so it shows no limit of the endpoint
itself; structure writes were not attempted. The 7.40 system has none of these endpoints.

### 0.16.1 release checks (0.16.1-rc.2, 0.16.1-rc.4)

The rc.2 runs are from 2026-11-19 and the rc.4 runs from 2026-11-20. rc.2 has the full feature set. rc.3 was
never published (a test adapter failed in CI). rc.4 is rc.2 plus one transport fix: RFC SADT requests that
name no `Accept` header now send `Accept: */*`. The 0.16.1 release binaries are the rc.4 binaries. The
systems are the 7.50 system, the 7.40 system and the 758 system (S/4HANA).

| System | Run | Result |
| :--- | :--- | :--- |
| 7.50 | rc.2, full matrix, macOS arm64 | 541 PASS, 3 FAIL |
| 7.50 | rc.2, group m7 (0.16.1 cells) | 19 PASS, 0 FAIL |
| 7.50 | rc.2, smoke | 48 PASS, 0 FAIL |
| 7.50 | rc.4, smoke | 48 PASS, 1 UNSUPPORTED, 0 FAIL |
| 7.40 | rc.2, full matrix, Linux x64 | 267 PASS, 3 FAIL |
| 7.40 | rc.2, group m7 | 15 PASS, 0 FAIL |
| 7.40 | rc.2, smoke | 35 PASS, 0 FAIL |
| 7.40 | rc.4, smoke | 35 PASS, 16 UNSUPPORTED, 0 FAIL |
| 758 | rc.2, group m7 | 24 PASS, 4 UNSUPPORTED, 0 FAIL |
| 758 | rc.2, smoke | 46 PASS, 2 FAIL |
| 758 | rc.4, group m7 | 21 PASS, 4 UNSUPPORTED, 3 FAIL |
| 758 | rc.4, smoke | 48 PASS, 1 UNSUPPORTED, 0 FAIL |

- The 3 FAIL cells of each full matrix are older cells whose expectations predate 0.16.1.
  They are not product defects. On the 7.50 system, the m6-ddic group (67 PASS, 0 FAIL) and the MCP group on
  the RFC SDK (8 PASS, 0 FAIL) were rerun after a network timeout and a hung run, and passed.
- The 2 FAIL cells of the rc.2 smoke on the 758 system were the `doctor` runner leftover search over RFC
  (RFC SAP GUI library and RFC SDK). It answered HTTP 400 because the request had no `Accept` header. rc.4
  fixes this; the leftovers line passes on every path that runs classrun (all three paths on the 7.50
  and 758 systems, the two RFC paths on the 7.40 system).
- The 3 FAIL cells of rc.4 group m7 on the 758 system are the `doctor` orphan cell. Its fixture, a runner row
  that exists only in `TADIR`, was deleted after the rc.2 check passed, as planned, so the cell has nothing to
  find.
- The 758 system was covered by the smoke set and group m7 only. The full matrix on it is planned after
  this release.
- No test object was left on any system after any run.
- Known gaps. `--tr` adding an object that is not yet recorded has no live test: no `YTEST` object can reach
  that state without releasing a test request. The 7.40 system has no stateful ADT HTTP session, so classrun
  there runs over RFC only (unchanged). Issue #13 (a data preview with a single-value `=` condition reported
  as unreliable on the 758 system) is out of scope: ABAPReach's own queries use `IN`.

### 0.17.0 release checks (0.17.0-rc.1 to rc.4)

The runs are from 2026-11-21 and 2026-11-22. The 0.17.0 release binaries are the rc.4 binaries. rc.4 differs
from rc.3 only in one more known dump text: the ADT HTTP page of the 758 system for a report that sends a
screen. rc.3 differs
from rc.2 only in the `run` reply mapping: a report that sends a screen now reads `RUN_FAILED` where the
reply carries the dump short text. rc.2 differs from rc.1 only in fault-injection code (`hold-post`); the
macOS release binary of rc.1 and rc.2 is byte-identical. The systems are the 7.50 system, the 7.40 system
and the 758 system (S/4HANA). All runs use the live harness on the three paths: ADT HTTP, RFC through the
SAP GUI library and RFC through the NW RFC SDK.

The full matrix runs once per release on each system. A later release candidate reruns only the cells its
change touches.

| System | Host | Run | Scope | PASS | UNSUPPORTED | FAIL |
| :--- | :--- | :--- | :--- | ---: | ---: | ---: |
| 7.50 | macOS arm64 | rc.2 | full matrix, 3 paths | 711 | 68 | 0 |
| 7.40 | macOS arm64 | rc.2 | full matrix, 3 paths | 491 | 288 | 0 |
| 758 | macOS arm64 | rc.1 | full matrix, ADT HTTP | 197 | 61 | 9 (explained below) |
| 758 | macOS arm64 | rc.1 | full matrix, RFC SAP GUI library | 220 | 44 | 2 (explained below) |
| 758 | Linux x64 | rc.2 | full matrix, RFC SDK | 171 | 35 | 0 |
| 758 | macOS arm64 | rc.2 | group m6-runner, ADT HTTP | 11 | 0 | 0 |
| 758 | macOS arm64 | rc.2 | group m6-runner, RFC SAP GUI library | 12 | 0 | 0 |
| 758 | macOS arm64 | rc.2 | groups m6-ddic and m6-delete, ADT HTTP | 67 | 5 | 0 |

- Why the 758 runs are split. The first full runs on the 758 system were rc.1 (ADT HTTP and RFC SAP GUI
  library). They had FAIL cells that rc.2 resolved, and the cells were rerun on rc.2 instead of repeating the
  full matrix. The RFC SDK path ran on Linux x64 on rc.2. Two outages of the network connection to the 758 system (a
  refused connection on the local port) cut runs on 2026-11-21 and 2026-11-22. The cut runs are kept as
  superseded logs and are not counted.
- The 11 FAIL cells of the rc.1 runs on the 758 system:
  - 7 cells of the ADT HTTP run were harness faults, not product defects. The harness helper that fills the test
    class posted the classrun from the session that had activated the class, and on the 758 system that
    session answers `Class does not implement if_oo_adt_classrun~main method!`; ABAPReach itself posts the
    classrun from a new session (see [Getting started: Run, Results](getting-started.md#results) and the
    0.16.1 entry of the [changelog](../CHANGELOG.md)). The harness also expected a conversion check where
    the 758 system has none (no `DD_DECIDE`, see below). Both are fixed in the harness. The rerun on rc.2
    (the m6 rows above) has 0 FAIL.
  - 4 cells were Ctrl-C timing cells (2 on ADT HTTP, 2 on the RFC SAP GUI library). rc.2 adds the fault `hold-post`, which pauses
    the runner after the classrun POST so that the Ctrl-C lands after it (see
    [Getting started: Fault injection](getting-started.md#fault-injection)). The rerun (m6-runner rows)
    has 0 FAIL.
- UNSUPPORTED cells of the 7.40 system (288): ADT HTTP is stateless there (160 cells), the system holds
  `$TMP` objects only, so no `--tr` or transportable-package cell can run (42), plus the cells that need
  the fault-injection ADT path, a second user, a 758-only fixture or a helper function module that is not
  installed there.
- Windows x64 and Linux arm64 had no live run in 0.17.0 (as in 0.16.1). CI builds and checks the release
  files for both.

#### 0.17 feature live cases

The cells of the 0.17 features ran as scripts: the Replay set on the rc.3 binary, the Extra set on the rc.4
binary. They use `$TMP` and `YTEST_ABAPREACH*`
objects only; every object was deleted and proved absent from a fresh session.

| System | Replay (0.17 cases) | Extra (8 cases added on 2026-11-22) |
| :--- | :--- | :--- |
| 7.50 | 66 PASS, 0 FAIL | 70 PASS, 0 FAIL |
| 7.40 | 53 PASS, 0 FAIL | 64 PASS, 0 FAIL |
| 758 | 61 PASS, 1 FAIL (script expectation, see below) | 70 PASS, 0 FAIL |

The Replay set covers:

- `run` of a class (classrun), of a report with parameters, and with an unknown parameter;
- ALV capture (`CL_SALV_TABLE`, `REUSE_ALV_GRID_DISPLAY`, `--alv-columns all`; hidden columns are not
  printed);
- `run SNIPPET`, and a snippet syntax error with its line;
- `aunit` of a program (one test passes, one fails) and of a class without tests;
- `inactive`, `activate --simulate`, `activate`, and a second `activate` that is refused;
- an inactive object made by the faults `drop-reply:PUT` and `skip-activate` (the 7.50 system, ADT HTTP and
  RFC);
- `delete FUGR` on the 758 system;
- the `TABL` conversion check (7.50 and 7.40 systems) and its refusal on the 758 system;
- `doctor` with `RUNNER leftovers: none`.

The Extra set covers:

- a report that sends a screen: `RUN_FAILED` on the 7.50 and 758 systems, where the reply carries the
  English dump text; on the 7.40 system the RFC connection closes without it and the run reads
  `RUN_UNKNOWN`;
- a snippet that raises an exception: `RUN_FAILED: exception CX_SY_NO_HANDLER ...`; the rollback is proven
  (the inserted row is absent, and the row of a control run is present). The table used for the proof is
  `YTEST_ABAPREACHL`, 16 characters, because a transparent table name has at most 16 characters;
- a report changed after the simulate: `REJECTED: plan changed since simulate (source hash); simulate again`;
- a run longer than 60 seconds on ADT HTTP (7.50 and 758 systems), on the RFC SAP GUI library and on the
  RFC SDK;
- `--timeout` per request: on ADT HTTP the run reads `RUN_UNKNOWN` (lost reply); over RFC it is refused
  with `STOP: --timeout is not supported over RFC ...`;
- a class `aunit` with one passing and one failing method;
- a parameter value of 45 characters (accepted) and of 46 characters (refused).

The one Replay FAIL on the 758 system is an outdated expectation of the script: it expected a source hash
after `create FUNC`, but a create without a source makes an empty skeleton and prints no hash (as `create
PROG` does). The output `CREATE: ACTIVE` is correct.

#### Differences between the three systems

- Classrun POST from a new session. On the 758 system the session that activated a class answers `Class does
  not implement if_oo_adt_classrun~main method!`, so ABAPReach posts the classrun from a new session (see
  [Getting started: Run, Results](getting-started.md#results)). The harness fill helper did not, which
  caused 7 of the rc.1 FAIL cells.
- No `DD_DECIDE` on the 758 system. A `TABL` change that needs the conversion verdict ends in a clean
  `REJECTED: The conversion check of <table> is not available on this release (no function module
  DD_DECIDE)`. The 7.50 and 7.40 systems return the verdict.
- A report that sends a screen. The 7.50 and 758 systems send an English dump text with the reply, so the
  run reads `RUN_FAILED: runtime error; see ST22 (dumps)`: `Screen output without connection to user` on
  7.50 (both backends) and on 758 over RFC, and `Sending of dynpro <program> <screen> not possible: No
  window system type specified` on the 758 ADT HTTP page. On the 7.40 system the RFC connection closes
  without it, and the run reads `RUN_UNKNOWN`.
- ADT HTTP is stateless on the 7.40 system, so `run` and the other runner operations use RFC there
  (unchanged).

#### No live test in 0.17.0 (covered offline)

These have no live cell. Offline tests cover them: Ctrl-C twice during `run` (the harness covers Ctrl-C
twice for `create`, not `run`); the server-side `BASE` check (reachable only by a change between the client
read and the POST); `RUN_UNKNOWN` from an ICM 5xx (a recorded body); the output budget with one-character
lines and non-ASCII text; an expired payload re-posted; a snippet with `ENDMETHOD` or `INCLUDE` in a colon
chain; a name that is not Z or Y without `--allow-standard`; a plan mismatch; the MCP policy matrix (`run:
off` with `mcp_run: confirm`, `mcp_run: off`, a name missing from `mcp_run_allow`, an aunit batch with one
object outside the allow list); and the selection-variant cell (it needs a variant made once in SE38 on
each system).

### 0.17.1 release checks (0.17.1-rc.1)

The runs are from 2026-11-25. 0.17.1 is the test and fault-build follow-up of 0.17.0, and the release
binaries are `v0.17.1-rc.1` (a single release candidate). The ordinary build behaves as in 0.17.0. The
fault-injection build's `hold-post:<ms>` also prints `FAULT: classrun POST sending` before the classrun POST
request starts. CI on rc.1 is green on all four runners (macOS arm64, Linux x64, Linux arm64, Windows x64).

| System | Path | Run | Result |
| :--- | :--- | :--- | :--- |
| 7.50 | ADT HTTP | Ctrl-C twice during `run` | PASS |
| 7.50 | RFC SAP GUI library | Ctrl-C twice during `run` | PASS |
| 7.50 | RFC SDK | Ctrl-C twice during `run` | PASS |
| 758 | RFC SDK | Ctrl-C twice during `run` | PASS |

The Ctrl-C cell ran from macOS arm64.

| System | Host | Run | PASS | UNSUPPORTED | FAIL |
| :--- | :--- | :--- | ---: | ---: | ---: |
| 7.50 | macOS arm64 | smoke | 48 | 1 | 0 |
| 7.50 | Linux x64 | smoke | 48 | 3 | 0 |
| 7.50 | Linux arm64 | smoke | 33 | 18 | 0 |
| 7.50 | Windows x64, native virtual machine | smoke | 48 | 1 | 0 |

- Ctrl-C twice during `run` (the 0.17.0 section lists it as covered offline only). The cell runs the
  snippet `WAIT UP TO 60 SECONDS` on the fault build. The first Ctrl-C comes on the POST marker, the second
  15 seconds after the waiting text. On every path above the command exits with 130, prints one
  `RUNNER_LEFTOVER: <class>` line with the two delete commands, and prints no outcome line. `doctor` lists
  the class, and the two commands remove it. Limit: that the POST had reached SAP rests on the 15 second
  delay (the POST session logon measured about 1 to 2 seconds), not on an observation.
- The smoke runs are the first live Windows x64 and Linux arm64 runs since 0.16.1 (0.17.0 had none). They
  are smoke only, not the full matrix. The UNSUPPORTED cells are structural: the ADT HTTP path loads no RFC
  library, and the OS trust store is already installed. SAP GUI for Java runs on x86_64 only, so Linux
  arm64 has no RFC SAP GUI library path.
- The 7.40 system was not run for 0.17.1.

### 0.18.0 release checks (0.18.0-rc.2 and rc.3)

The runs are from 2026-11-26 and 2026-11-27. The 0.18.0 release binaries are the rc.3 binaries. rc.3 differs
from rc.2 in two changes: `dump` stops on a single-byte RFC library, and server host values are not treated as
secret fragments. rc.2 differs from rc.1 in the read of the `RFC_SYSTEM_INFO` character fields of `ping`
(`RfcGetChars`). Cells that rc.3 does not change keep their rc.2 result: of the cells that carry a binary
version, 4844 ran on rc.3 and 1380 on rc.2; the other 2073 cells are UNSUPPORTED or housekeeping cells and
carry none. The three systems are the 7.50 system, the 7.40 system and the 758 system (S/4HANA), all on the
three paths: ADT HTTP, RFC through the SAP GUI library and RFC through the NW RFC SDK. The merged result has
290 scenarios (282 path scenarios and 8 housekeeping scenarios: sweeps, object setup and cleanup, runner
prefix, credential permission) in 8297 cells; where a cell ran more than once, the last result counts.

| System | Hosts | PASS | UNSUPPORTED | FAIL |
| :--- | :--- | ---: | ---: | ---: |
| 7.50 | macOS arm64, Linux x64, Linux arm64, Windows x64 native, Windows x64 emulated on arm64 | 2933 | 519 | 4 |
| 7.40 | the same five hosts | 2015 | 1265 | 1 |
| 758 | macOS arm64, Linux x64 | 1289 | 271 | 0 |
| All | | 6237 | 2055 | 5 |

The cells of the 0.18.0 features (group m8) are 648 of these cells: deploy by `edits` (5 scenarios,
`m8-edit-*`) 145 PASS and 35 UNSUPPORTED; `ping` 34 and 2; `search` (3 scenarios) 102 and 6; `sql`
(4 scenarios) 136 and 8; `where-used` (3 scenarios) 97 and 11; `dumps` and `dump` (2 scenarios) 57 PASS, 1 FAIL
and 14 UNSUPPORTED. All of them ran on rc.3. The per-difference results are under differences 12 to 15.

- Parallel lanes and the serial reruns. The first runs on rc.2 and rc.3 ran several lanes of one SAP user
  in parallel and had cells that failed in setup, in the generated runner class or in cleanup. On
  2026-11-27 those cells were rerun on rc.3, one lane at a time per system (7.40 and 7.50 systems): 34 cells,
  all PASS. Nine of them were FAIL before the rerun
  (a helper lock refused right after a create, a setup object that did not end active, an activation error of
  the generated runner class, a read-back that was not the new object, or a live definition that was not the
  deployed file). The two cleanup cells of the 7.40 system on Windows x64 emulated passed in the same
  reruns. `m6-dtel-inactive` was rerun serially on the 7.40 system, RFC SDK, Linux arm64, and passed on
  rc.3.
- No product defect is open. The 5 FAIL cells that remain:
  - `m6-dtel-inactive` on the 7.40 system, RFC SDK, macOS arm64, rc.2: the assertion `inactive-created`
    failed with the runner activation error `LINE 10035 ... (RFC code 1)` and the runner cleanup proof
    reported that the class still existed in a new session. It is the signature of the parallel-lane runner
    errors above (several lanes of one SAP user). The same cell passed in the serial rerun on Linux arm64,
    on RFC through the SAP GUI library on the macOS host (rc.3), on the 7.50 system and on the 758 system.
    The cell was not rerun on macOS arm64, so the FAIL stays in the merged result.
  - `object-cleanup`, `object-cleanup/transport` and `sweep-end` on the 7.50 system, macOS arm64 (3 harness
    housekeeping cells of one run): the cleanup verification and the end sweep of that run failed. The later
    7.50 runs on other hosts and the serial rerun pass these cells.
  - `m8-dump-show` on the 7.50 system, RFC SDK, Windows x64 native: a harness artifact, described under
    difference 13. The full run of the cell passed.
- Main UNSUPPORTED reasons (counts are cells). 7.50 system (519): RFC through the SAP GUI library on Linux
  arm64 (214; SAP GUI for Java for Linux is x86_64 only), fault injection (45; it drops RFC replies only, no
  ADT HTTP fault), cells that need the 758 system (28), ADT HTTP cells for a check of the RFC library loader
  (17), and cases that a live run cannot produce, which only an offline fake models (14 per case). 7.40 system
  (1265): ADT HTTP has no stateful session (339 cells run in their pinned and fallback form only, and 214
  more need the temporary runner class), RFC through the SAP GUI library on Linux arm64 (214), `$TMP` only so
  no `--tr` cell can run (132), fault injection (38), no second developer user (34), the helper function
  module of the reference tool is not installed (24), and cells that need the 758 system (24). 758 system
  (271): `$TMP` only (60), no second developer user (24), no transportable package (24), the helper function
  module of the reference tool is not installed (24), fault injection (18) and ADT HTTP cells for the RFC
  library loader (7).
- The 758 system ran on macOS arm64 and Linux x64 only; Linux arm64 and Windows x64 had no run on it.

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

### A transportable object whose lock reports no transport request needs `--tr`

When the lock of a transportable object (package not `$TMP`) answers 200
without a `CORRNR`, no modifiable request holds the object. Without `--tr`,
`abapreach` stops with `STOP: the object is not local but the lock returned no
transport request; pass --tr <request> to add the object to a modifiable request
of yours` (exit 2) after releasing the lock and before the syntax check and PUT.

With `--tr <request>` (0.16.1), `abapreach` first reads the request from `E070`
over the data preview (read-only) and stops, writing nothing, unless the
request exists, is a workbench request and not a task, is modifiable, and is
owned by the logon user or holds a modifiable task of that user. These are the
rules the classrun runner applies to its `--tr`. The write then carries
`corrNr=<request>` and SAP records the object in that request; `deploy` prints
`transport request <request> (object added)`, and `deploy --simulate` prints
`WILL ADD <TYPE> <NAME> to <request>`. `delete` over ADT follows the same rule at
execute time and prints no `object added` line. A `CORRNR` that the lock does name must
still equal `--tr`.

The reference tooling would still write there. For a transportable object it
requires `--tr`, checks that the request is a modifiable workbench request
owned by the user (`E070`) and that the object is not enqueued in another
request (`TLOCK`), then passes `--tr` as `IV_TRKORR`; the function module
locks with exactly that request (`set_lifecycle_corr`: `mo_lm->corrnr =
mv_trkorr`). The request of the write is therefore chosen by the client,
not read from the object lock.

`abapreach` does not read `TLOCK`: an object that another request already
holds answers a `CORRNR` at the lock, so this path is reached only for an
object that no open request holds. A write that SAP refuses for a wrong
request is reported as the SAP answer.

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

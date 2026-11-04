# Behavior differences from the Python tooling

The Rust CLI is a partial port of the earlier Python tooling. This page
lists known intentional behavior differences for operations available in
the Rust CLI. It is not a feature-parity claim. See the [roadmap](roadmap.md)
for current scope.

Each entry names the live matrix scenario that exercises it, where one exists. An entry
marked "not exercised" describes implemented behavior that the release-candidate
matrix does not test.

## Intentional differences

### 1. ADT delete transport selection happens at execute time

ADT simulation is read-only and cannot report which request currently owns a transportable
object. The execute-time LOCK reply decides the request; an explicit `--tr` must match it.
Matrix: not exercised (requires a transportable delete fixture).

### 2. Function modules use the ADT server-rendered format

The source of a `FUNC` is the ADT format: `FUNCTION <name> IMPORTING ... .` followed by the body.
The Python tooling uses the `*"` interface header in the file. A Python-format file is refused with
exit 2 (`STOP`). After a PUT, SAP re-renders the signature; ABAPReach compares the text of the PUT
reply, not the file bytes. Matrix: `func-python-file-rejected`, `func-deploy`, `bom-crlf-deploy`.

### 3. `CLAS` simulation does not save

`deploy CLAS --simulate` runs the syntax check through the checkrun and does not save. The matrix
checks that the active and inactive source are unchanged and that the output has one `live hash`
line. The Python function module writes the inactive version and then discards it. The transport
request entry is recorded as unobserved. Matrix: `clas-simulate-no-save`.

### 4. Source-free create

`create` makes an empty object (a skeleton) from the name, the text and the package or function
group. The Python `create` takes the file as an argument and writes the source with it. In
ABAPReach the source comes with `deploy`, after `create` and a simulation. Matrix: `create-*`
and `create-simulate`.

### 5. Uncertain create tokens

When a create POST outcome is unknown, ABAPReach reads the object and reports one of:

- `CREATE: PARTIAL` (exit 1): the object exists, or it cannot be read.
- `CREATE: REJECTED` (exit 1): the object does not exist; check the system before another run.

The Python tooling prints `CREATE status unknown` with no status token and does not read the object
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

Names for `create` must start with `Z` or `Y`. The Python tooling checks a single name limit of
40 for all kinds; its text limits are the same as above. Not exercised: the live matrix does not run the name or text limits.

### 7. `read --lines` writes to stdout only

`read ... --lines A-B` requires the output to be `-`. Any other value stops with exit 2
(`STOP: --lines requires OUT to be '-' (stdout)`). The Python `rfc-read source --lines` prints to
stdout with no output argument. The full source is read and hashed before the range is taken.
Matrix: `read-lines` and `read-lines-grammar`, both on `FUNC` only.

### 8. `--create-include`

The Python ADT tooling (`deploy_hunks.py`) stops at an empty include. ABAPReach stops
with `INCLUDE_MISSING: <kind> (re-run with --create-include)`, exit 1, before any write. With
`--create-include` it sends one POST that creates the include, inside the class lock, and never
repeats it. The Python `rfc-deploy` has no local-include writer at all. Matrix: `include-missing`
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

## Observed parity notes

These results come from a run that deployed the same change with the Python tooling and with
ABAPReach to twin objects on the SAP 7.50 system. The final run gave 30 PASS, 6 EXPECTED and no
FAIL. It did not compare on the SAP 7.40 system, because the Python deploy function module is not
installed there.

- **Local includes.** The Python `rfc-deploy` has no local-include writer, so the `CLAS` with
  `CCIMP` row has no Python side (EXPECTED).
- **Change after the final `ENDCLASS`.** The Python tooling refuses a change appended after the
  final `ENDCLASS` of a class (`Active source content ... changed when inactive was discarded`).
  ABAPReach deploys it and reports `ACTIVE` only after the active class source it reads back equals
  the file. A reread with either tool afterwards shows the appended text in the active source.

## Transport requests, inactive versions and related behavior

### Inactive version after a failed activation

The Python function module (`YFM_UTILS_SOURCE_DEPLOY`) discards the inactive
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

The Python tooling would still write there. For a transportable object it
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
  another user's inactive version, matching the Python function module's
  verdict. The Python tooling always refuses; the `allow` and `ask`
  policies exist only in `abapreach`.
- `deploy --simulate` writes nothing in both tools (a syntax check only);
  the active hash, inactive entries, and enqueue table are equal afterward.
  For `CLAS`, see difference 2.
- Both Rust backends compute `--base` from the normalized active source, the
  same way as the Python tooling. A live comparison on SAP_BASIS 7.50 gave
  the same source and the same hash from Rust `read` and from the Python
  function module, so a hash from either tool is accepted by the other.
- `GET <source>?version=inactive` without an inactive version can answer
  200 with the active source, so reconciliation compares content rather than
  relying on that HTTP status alone.

### `read-table` against the Python `rfc-read table`

- `--rows 0` is refused (STOP, exit 2); the Python tool reads every row for
  `rows=0`. The ADT data preview has no "all rows" mode, so `--rows` is 1 to
  5000, the largest value the 7.40 data preview honours (MCP: 1 to 1000).
- The backend is the ADT data preview, on ADT HTTP and over RFC. The Python
  tool calls `RFC_READ_TABLE`; its 72-character `split_where` is ported
  (`split_where`, literal-safe) but not on the default path.
- `--where` accepts only the tokenized grammar: `field op literal` with
  `= <> < > <= >= LIKE IN BETWEEN IS [NOT] NULL`, `AND`/`OR`/`NOT` and
  parentheses. The Python tool sends the text through unchanged. Fields,
  second tables, `~`, `@`, comments, subqueries and joins are refused before
  any connection, because SAP runs a data preview body as written.
- A view is read only when it is a database view and none of its base tables
  (`DD26S`) is on the `read_table_denied` list. A name missing from `DD02L`,
  a failed dictionary lookup, a CDS/DDLS entity or another view type is
  `REJECTED` (exit 1) and nothing is read.
- Each command uses a fresh connection; the data preview over RFC fails on
  the 37th call of one connection.

## `delete` (shared operation, M4)

The Rust delete operation ports the cases of `tools/test_rfc_deploy.py` that apply to
the ADT route (`tests/delete_operation_test.rs`). Intentional differences:

- The plan hash also covers the active source of every source object (`CLAS` main
  source plus each local include, `FUNC`, `INTF`, `PROG`), or the marker `NO_ACTIVE`. The
  Python plan hash covers only the plan lines. Where-used is reported and never hashed.
- A real delete reads each object again and compares its plan hash itself, because there
  is no function module to do it. A changed object is `REJECTED` with "Plan changed since
  simulate; simulate again", and the batch continues.
- `foreign_inactive` applies to delete (`reject` by default; `ask` prompts on a terminal
  and counts as `reject` under MCP). Python has no such guard for delete.
- Not ported because there is no function-module call on the ADT route: the batch list
  (`IT_SOURCE`) sent to the function module, and the "empty plan hash" case (a hash is
  always computed locally).
- Not ported here because the CLI grammar is not part of this operation: duplicate-object
  and `--tr` syntax are checked by the operation, option combinations by `clap`.
- A lost delete reply is reconciled once from a new session and never retried
  (`DELETED (reconciled)` exits 1); Python reports an unknown status and stops.
- `delete --simulate` prints the guards that apply and the function-module guards that
  do not apply on the ADT route (system change option, `CCNOCLIIND`, SE06 and software
  component, server-side self-guard).

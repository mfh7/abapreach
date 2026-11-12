# Deploy safety

This guide describes what ABAPReach checks before and after it writes an existing `PROG`,
`FUNC`, `CLAS` or `INTF`, and what it checks when it creates an object. The DDIC types and
transaction codes have their own rules, in [DDIC and TRAN](#ddic-and-tran). It is not a substitute
for reviewing your local change and your SAP transport state.

## Recommended sequence

1. `read` the object and keep the printed `hash`.
2. Review your local edits and the intended SAP target.
3. Run `deploy ... --simulate --base <hash>`.
4. Review the result, then run the deploy command with the same `--base`.
5. If the outcome is uncertain, use `read` and inspect SAP state before a retry.

`--simulate` runs through the syntax check without saving and releases its
lock. A deploy that reaches comparison prints a unified diff and the live
hash. `--base` checks that the active source still has the expected hash,
including after the lock. `--backend adt|rfc` selects a backend explicitly;
either `--backend` or `--base` prevents automatic fallback to another backend.

Once the lock is held, every exit path releases it in the same session: a
stop, a rejected check, a failed PUT, and Ctrl-C. If the release fails or its
result is unknown, the run stops before activation, names the lock that may
remain, and exits 1.

## Inactive versions owned by another user

The `foreign_inactive` policy controls whether deploy may overwrite another
user's inactive version. The default is `reject`. Define a global policy in
`connections.json`; `servers[].policies` overrides individual keys:

```json
{
  "policies": {"foreign_inactive": "reject"},
  "servers": [
    {
      "systemId": "A4H",
      "host": "sap.example.com",
      "port": 443,
      "clients": ["001"],
      "rfc": {"ashost": "sap.example.com", "sysnr": "00"},
      "policies": {"foreign_inactive": "ask"}
    }
  ]
}
```

| Value | Another user's inactive version |
| :--- | :--- |
| `reject` (default) | refused before the lock: `REJECTED`, exit 1 |
| `allow` | overwritten, with a warning |
| `ask` | prompt; only `y` or `yes` continues |

`ask` refuses when stdin or stderr is not a terminal.

The prompt names the program, inactive-version owner, and number of changed
lines. **The full diff is buffered until the command finishes, so it is not
visible before you answer.** Review your local change before using `ask`.

Deploy and simulation read `GET <source>/versions` before the lock and again
right after it. A version created by another user in between is refused after
the lock and the lock is released; `ask` does not prompt while the lock is
held. Under `reject` and `ask`, an unreadable or unparseable version list is
refused. Only `allow` continues with a warning. An inactive version owned by
the logon user does not block deploy. Unknown policy keys or values stop
configuration loading with exit 2.

The matrix ran the `reject` and `allow` scenarios, and the `ask` prompt scenarios, on an
interface (`INTF`) on the SAP 7.50 system. The second developer user exists only on that system,
so the foreign-inactive scenarios did not run on the SAP 7.40 system.

## Function modules

**Inactive version of the same function module.** A `FUNC` with an inactive version
of any owner, including the logon user, is refused before the PUT:
`REJECTED: Function module <NAME> has an inactive version; activate or discard it first in SE37`,
exit 1. Activate or discard that version in SE37, then deploy again. Matrix: `func-inactive-own`.

**Inactive version of another object in the same function group.** The deploy is not
refused. ABAPReach prints `W: inactive objects in function group <GROUP>: <list>` and a second
warning that the syntax check of this function module may fail because of them. Matrix:
`func-inactive-sibling`, which passed on ADT HTTP and both RFC paths on the SAP 7.50 system and
on the RFC paths of the SAP 7.40 system.

**Interface change.** ABAPReach compares the signature in your file (the text before the first
`.` after `FUNCTION`, with spacing and keyword case normalized) with the live signature. If they
differ, a deploy or simulation prints:

- `INTERFACE CHANGED` and a unified diff of the parameters;
- `WHERE-USED <TYPE> <NAME>` for each caller, up to 50 callers, then `WHERE-USED ... <n> more`;
  or `WHERE-USED: none` when there are no callers;
- `W: where-used unavailable: <reason>` when the callers cannot be read; the deploy continues;
- `W: callers' syntax check does not catch missing CALL FUNCTION parameters; they dump at runtime`.

The where-used list is advisory. It never refuses a deploy. Package entries (`DEVC`) are not
callers and are left out of the list, and a caller that appears twice is listed once. The
matrix scenario `func-signature-change` (SAP 7.50 system) checks that the list holds only caller
kinds and that no `more` line appears for its single caller. The 50-caller cap is implemented but
is not exercised by a matrix scenario. The where-used failure branch (`func-where-used-unavailable`) has no live evidence.
An unchanged signature prints no interface report (`func-signature-unchanged`).

## Classes, local includes and interfaces

**Simulation does not save.** `deploy CLAS --simulate` runs the syntax check and releases the
lock. Result: `SIMULATED_OK`, exit 0. Matrix: `clas-simulate-no-save`, which passed on the SAP 7.50
system over ADT HTTP and both RFC paths and on the SAP 7.40 system over RFC. The matrix checks that
the active and inactive source are unchanged afterwards and that the output has one `live hash`
line. The transport request entry is not observable through a read-only endpoint, so it is recorded
as unobserved.

**Local include that does not exist.** Before the lock, ABAPReach reads the class structure. If the
named include (`CCIMP`, `CCDEF`, `CCMAC` or `CCAU`) is not there, the deploy stops with
`INCLUDE_MISSING: <kind> (re-run with --create-include)`, exit 1, and writes nothing. Matrix:
`include-missing`.

**Creating the include with `--create-include`.** The flag is valid only with `CLAS --include`.
ABAPReach then sends exactly one POST that creates the include, inside the same class lock, then
the source PUT, the unlock, and one class activation. A POST is never sent a second time. Matrix:
`include-create` (`CCAU`), which passed on the SAP 7.50 system over ADT HTTP and over both RFC
paths (the SAP GUI library cell on Linux arm64 is UNSUPPORTED). The matrix checks the include, the
source and the lock after the run; it cannot count write POSTs, so the one-POST rule is verified by
the code path, not by a live count.

If the include POST outcome is uncertain, ABAPReach reads the include and classifies it. It never
repeats the POST:

| Include state after the uncertain POST | Result | Exit |
| :--- | :--- | :---: |
| does not exist | `REJECTED`: nothing written; the run can be repeated | 1 |
| exists, inactive source equals your file | `PARTIAL`: re-run without `--create-include` to activate it | 1 |
| exists, inactive source differs | `PARTIAL`, with the line `INCLUDE_SOURCE_DIFFERS` | 1 |
| cannot be read | `PARTIAL`: the include cannot be read; the run is not retried | 1 |

The lock is released in every branch. **These branches have no live evidence:** the matrix
records them as UNSUPPORTED, because a live run cannot produce an uncertain include POST. Only
the offline fake exercises them.

**Transportable class activation.** Activating a class in a transportable package takes two
steps. When the first activation answers with an inactive-objects worklist, ABAPReach sends
every worklist entry again, method entries included, without a pre-audit request. It does this
only when every entry is under the class URI. Matrix: `clas-activation-worklist`, which passed on
the SAP 7.50 system over ADT HTTP and the NW RFC SDK, and on the SAP GUI library path except on
Linux arm64 (no library). The product prints no line for the resend; the matrix confirms the outcome
from the active and inactive readback. The 7.40 system has no transportable objects, so the scenario does not
run there. The warning-only and error responses of that activation (`clas-activation-messages-w`,
`clas-activation-messages-e`) and worklist entries outside the class URI
(`clas-activation-outside-class`) are UNSUPPORTED: they have no live evidence.

## Create

`create` makes an empty object (a skeleton) without source. The source comes afterwards with
`deploy`, after a simulation. `create ... --simulate` writes nothing and prints
`CREATE_SIM: SIMULATED_OK`, exit 0 (matrix: `create-simulate`).

- A name that already exists is refused: `CREATE: REJECTED`, exit 1 (`create-existing-name`).
  A second run of the same create is refused the same way (`create-second-run-rejected`).
- `--tr` for a `$TMP` package and a missing `--tr` for a transportable package stop with exit 2
  (`create-tr-on-tmp`, `create-transportable-no-tr`).
- Name and text limits are checked before the create request is sent. `PROG`: name 40, text 70.
  Names start with `Z` or `Y`. See
  [Getting started](getting-started.md#other-source-objects). The matrix does not exercise them.
- `create PROG --subc I` creates an include; see [Program includes](#program-includes).

**Uncertain create.** When the create POST outcome is unknown, ABAPReach reads the object (a
read-only GET) and does not send the POST again:

| Object after the uncertain create | Result | Exit |
| :--- | :--- | :---: |
| exists | `CREATE: PARTIAL` | 1 |
| cannot be read | `CREATE: PARTIAL`; the run is not retried | 1 |
| does not exist | `CREATE: REJECTED`; check the system before another run | 1 |

Matrix: `create-uncertain-present` and `fault-create-drop` (drop of the CREATE reply) and
`create-uncertain-unreadable` (drop of the CREATE reply and failed reconciliation read); the RFC paths
passed, the ADT HTTP cells are UNSUPPORTED because fault injection drops RFC replies only. The
`does not exist` branch (`create-uncertain-absent`) has no live evidence.

**No automatic rollback.** A failed or uncertain create is not rolled back. An object that was
created and left behind stays in SAP until someone removes it. Deleting objects is covered in
[Delete](#delete); there is no restore.

## Program includes

A program include (`PROG` with `--subc I`) is read, deployed and deleted as a `PROG` name. Its
checkrun and activation need a main program as context, which is either `--main <PROG>` or the
single program that the include's `mainprograms` lists.

- Read needs no main. `--main` on a program is refused (`STOP`, exit 2).
- Deploy and simulate print `main <PROG>`. More than one listed program with no `--main`, or none
  listed with no `--main`, is a `STOP` with exit 2; the text names the programs found.
- A given `--main` must exist as a program and its active source must hold the `INCLUDE <I>.`
  statement. Otherwise the command stops with exit 2: `main program <P> does not exist as a program`
  or `main program <P> does not contain the statement INCLUDE <I>.`.
- The include is never activated without its main program as context. A standalone activation
  would skip the check, so the contextual activation is the check.
- An include whose checkrun answers `notProcessed` is `REJECTED` (exit 1, `REJECTED: the check was
  not processed: <statusText>`), with one exception: a new include, whose `mainprograms` is empty and
  whose `--main` is given. Its first simulate prints
  `W: no syntax check before the first activation of a new include (main <PROG>)` and the deploy
  continues; the contextual activation is then the check. The exception is decided from the empty
  `mainprograms`, not from the SAP status text.
- A worklist on an include activation ends `INACTIVE` (exit 1). It is not sent again.
- The first-activation warning is covered by offline fixtures only. On the matrix systems, activating
  a parent that holds `INCLUDE <name>.` always fills the index, so the state never arose live.
- A transportable package carries `corrNr` on every request (see
  [Transport requests and failed activation](#transport-requests-and-failed-activation)). Creating
  in a transportable package without `--tr` is refused before any request.
- Delete: a program is deleted before the includes of its batch. An include that a program still
  references is refused by SAP (HTTP 403, `Program <N> is referenced in other programs`). Program and
  include where-used run over ADT HTTP and over RFC SADT. A transportable include delete over RFC
  SADT has fixture coverage only; the 0.15.0 matrix has no live run of it.

## Lost create and delete replies

A `create` or `delete` whose reply is lost is settled by one read from a new
connection. It is never sent again, and no rollback is attempted. Exit codes:
- `CREATE: PARTIAL` (exit 1): the object is found, or the read fails.
- `DELETED (reconciled)` (exit 1): a new connection no longer finds the object.
- `PARTIAL` (exit 1): the state is unknown.

An uncertain create ends its message with `the create is not repeated; check with read before
repeating; a lock may remain`. An uncertain delete prints `W: reply lost (<reason>); a new session
no longer finds the object: check before retrying` for `DELETED (reconciled)`, or `W: status
unknown: reply lost (<reason>); ...` for `PARTIAL`, and then `W: a lock may remain on the object`.
Read the object before any retry.

## Transport requests and failed activation

For a transportable object, ABAPReach relies on the `CORRNR` returned by the
SAP lock. If the lock returns no request, it stops before syntax check or PUT,
even when `--tr` was supplied. It also refuses a mismatched `--tr`. See
[Behavior differences](behavior-differences.md#a-transportable-object-whose-lock-reports-no-transport-request-is-refused)
for the difference from the earlier internal tool.

If activation fails after a PUT, the failed source may remain as the
program's inactive version while the active source stays unchanged. The tool
reports `INACTIVE` with exit 1. Inspect the inactive version before another
deploy; do not assume a failure rolled the write back.

## Uncertain write outcomes

A timeout or lost connection after a PUT or activation request leaves the
write outcome uncertain. ABAPReach does not retry that write or switch
backends. It only reads the active and inactive source and compares them with
the local file. Possible reports are `ACTIVE (reconciled)`, `INACTIVE`,
`PARTIAL: outcome unknown`, `PARTIAL: unexpected state`, and
`PARTIAL: reconciliation incomplete, inactive source unreadable (<reason>)`. **Every
reconciliation result exits 1**, including `ACTIVE (reconciled)`, and tells
you to check with `read` before retrying.

The same read-only rule applies to an uncertain create (`CREATE: PARTIAL` or `CREATE: REJECTED`,
see [Create](#create)) and to an uncertain `--create-include` POST (see
[Classes, local includes and interfaces](#classes-local-includes-and-interfaces)).

Do not automate a retry solely from the exit code. Re-read the object and
inspect its active and inactive state in SAP. If an inactive version remains,
decide how to restore or activate it deliberately. The tool does not perform
an automatic rollback.

The matrix covers the PUT and activation reply drops on the RFC paths: a dropped PUT reply
reports `INACTIVE`, a dropped activation reply reports `ACTIVE (reconciled)`, both exit 1, and
neither is retried. ADT HTTP has no fault injection.

## Delete

`delete` removes objects only from a plan written by `--simulate`. Simulate reports the objects,
their transport request and where-used references; no object is changed.

- **Confirmation.** A delete through MCP always needs a human confirmation: the typed object name
  (or `DELETE <n>` for a batch of n). In `token` and `confirm` mode alike. A client that cannot
  answer the form is refused. See [MCP server](mcp.md#mcp_write).
- **Reconciliation.** When the reply of a delete is lost, ABAPReach reads the object once from a
  new session and never sends the delete again. The result is one of:
  - `DELETED (reconciled)`: a new session no longer finds the object. The message says
    `check before retrying`, and the exit code is 1.
  - `PARTIAL`: the status is unknown. The object still exists, or the check failed. The message
    names a possible remaining lock. Read the object before any retry.
  The objects after the uncertain one are `SKIPPED`; the batch stops.
- **Backend.** ADT HTTP runs first. When ADT fails before anything is sent (connect, DNS, TLS),
  the batch runs again on RFC SADT with the usual note. After the first LOCK or DELETE is sent,
  there is no fallback. An MCP `plan_token` pins its backend and never falls back.
- **Sessions (ADT HTTP, 0.15.0).** A batch logs off the stateful session after each row it deleted
  and runs the next row in a new session (`W: session close failed: <reason>` if a logoff fails). A
  row that deletes nothing keeps its session. Within a batch, programs come before their includes.
- **Transport request.** An object in your own request follows that request. An explicit `--tr`
  that differs from the request of the lock is rejected. An object locked in another user's
  request is rejected, and so is a transportable object without a request. Transport-request
  checks run at execute time, because simulate does not lock the object.
- **Where-used of a program (0.15.0).** The simulate of a `PROG` lists the programs that include it,
  then the transactions that start it as `WHERE-USED TRAN <tcode>` (over ADT HTTP and over RFC SADT).
- **Rollback.** There is no automatic rollback and no restore step in this release. A deleted
  object is not restored by ABAPReach; restore is in the [roadmap](roadmap.md) (`Versions, restore,
  activation`).

## DDIC and TRAN

This section covers the data elements (`DTEL`), domains (`DOMA`), tables (`TABL`: transparent tables
and structures), table types (`TTYP`) and transaction codes (`TRAN`) of 0.16.0. They run on the 7.50
and 7.40 systems through a temporary class (classrun). Nothing is installed on the SAP system.

**Footprint.** Every operation on these types, `read` and every simulate included, creates one
temporary class in `$TMP`, runs it and deletes it. The `TADIR` entry of that class is created and
removed; no transport request entry is written for it. A deploy does it twice: run 1 shows the diff
and the live hash, run 2 writes. Each result prints a `RUNNER:` line with the class name and the
transport. If the class cannot be removed, the result carries `RUNNER_LEFTOVER` and exits 1; the text
names the class and the two commands that remove it.

If a run dumps on the server, the short dump stays in transaction `ST22`. The result reports the
failure. Read the dump in `ST22` before any retry. The rc.6 matrix left no new dump on either system.

**Modifiable system (SE06).** A run creates a class, which is a change to the repository. A system
whose system change option does not allow repository changes is refused before any class is
created: the preflight stops with exit 2 and the message
`<operation> <TYPE> needs a temporary class; repository changes are not allowed on <system>/<client> (<reason>)`.
The reason is the SE06 setting or the client setting. Client restrictions such as `CCNOCLIIND`
and productive clients are refused the same way. `doctor` prints the preflight result for each
connection. Ask the system owner before you run these types on a productive system: the temporary
class is a repository change, and an audit can see it.

**Authorization.** The run uses the authorization of the logged-on user; nothing is escalated.
The prerequisites are:

- `S_DEVELOP` for `CLAS` in `$TMP`: create, change and activate of the temporary class;
- `S_ADT_RES` for the classrun URI;
- DDIC and CTS authorization for the target object.

A refusal by SAP on the class create, activation or classrun POST (HTTP 403, `no authorization`) is
`REJECTED`, exit 1, with the SAP text and the hint `the temporary runner class needs CLAS create/activate
in $TMP and ADT classrun access`. There is no fallback to another path. `doctor` checks that the
handlers exist, not the authorizations, and prints `authorization not probed`.
**Gap:** the rc.6 matrix ran as one user with full authorization. No restricted user was available,
so the authorization refusal is covered only by offline fixtures.

**Deploy.** Deploy of `DTEL`, `DOMA`, `TABL` and `TTYP` follows the sequence at the top of this guide:
simulate with `--base`, review the diff, then deploy. After activation the file is compared with the
active definition. The master language fields (`DTELMASTER`, `DOMMASTER`, `MASTERLANG`) do not fail
that check when the file leaves them empty; SAP fills them, and the result carries the note
`W: active definition differs from the file (SAP canonical); run read again before the next edit`.
Read the object again before the next edit, so the file holds the canonical form.

**Conversion.** The verdict is the one SAP activation takes (`DD_DECIDE`), printed as a
`CONVERSION` line in the simulate output. A change SAP applies directly, such as a new field, is
activated. A change that needs an SE14 conversion is refused before anything is written, for example
a shorter field or a changed type on a table that holds rows, and also removing a field, even from an
empty table (rc.6 path comparison). Changing the table class (`TABCLASS`) is refused. A data element change that
reaches a transparent table prints a `WHERE-USED` line in the simulate output.

**Create.** `create` of a DDIC object needs the XML as `FILE`. `create TRAN` needs `--prog <REPORT>`.
A name that exists is `CREATE: REJECTED`. A failed activation after the create is `CREATE: INACTIVE`
(exit 1): the object exists and is inactive. A failed insert of a transaction code removes the
`TADIR` row it wrote; if that cleanup fails, the result is `CREATE: PARTIAL` (exit 1) and names the
left-behind `TADIR` entry. The uncertain-create rules of [Create](#create) apply.

**Delete.** A batch runs in this order: `TRAN`, `TTYP`, `TABL`, `DTEL`, `DOMA`. Before any delete, the
simulate checks that the object is not in use, that a table holds no rows, and that the plan is still
the same at execute time. Each refusal is `REJECTED` and the batch continues with the next object. An
object locked in your own transport request follows that request; an object in another user's request
is rejected. A transportable object needs `--tr`.

**Limits.** The file size limit is 256 KiB. `TABL` writes cover transparent tables and structures only.

## Cancellation and SIGKILL

- A write cancelled while it runs ends at its safe point with exit 130: unlock and nothing
  further written. A cancel after the source PUT ends as
  `PARTIAL: the new source is saved inactive and nothing was activated`. Through MCP, the next
  tool result carries a `W:` line that ends with `check the object before retrying`.
- A cancel before the request starts sends nothing.
- **Session logoff (0.15.0).** Every ADT HTTP stateful session ends with a logoff
  (`GET /sap/public/bc/icf/logoff`) on every exit path: success, error and interrupt. This releases the
  enqueue locks that 0.14.0 left in SM12. A failed logoff prints `W: session close failed: <reason>`;
  the status and exit code stay as they were, and the logoff is not retried. A call that never opened
  a session sends no logoff. RFC SADT is unchanged.
- SIGKILL cannot be caught. A write killed while it holds an ADT lock can leave that lock on the
  object. Check the lock in transaction `SM12` before any retry. This release did not test
  clearing it by hand.

## Status and exit summary

| Result | Exit | Meaning |
| :--- | :---: | :--- |
| `ACTIVE`, `SIMULATED_OK`, `no changes` | 0 | done |
| `CREATE: ACTIVE`, `CREATE_SIM: SIMULATED_OK` | 0 | created, or simulated without a write |
| `INACTIVE`, `REJECTED`, `PARTIAL` | 1 | inspect the SAP state |
| `CREATE: REJECTED`, `CREATE: PARTIAL` | 1 | inspect the SAP state; read before retrying |
| `INCLUDE_MISSING: ...` | 1 | the class include does not exist; nothing written |
| `NOT_FOUND: PROG <N> ...` | 1 | no program or include of that name; nothing written |
| `DELETED (reconciled)`, `PARTIAL` (delete) | 1 | inspect the SAP state; read before retrying |
| `ACTIVE (reconciled)` | 1 | file is active; verify with `read` |
| `STOP: ...` | 2 | input, configuration, or guard |

Exit code 3 is not used by this release.

The final backend and guard report appears in deploy output. `read` reports
line count and hash, not a backend/guard report. `doctor` has its own exit
meaning: 0 only when all its probes pass, 1 otherwise.

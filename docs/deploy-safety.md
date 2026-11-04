# Deploy safety

This guide describes what ABAPReach checks before and after it writes an existing `PROG`,
`FUNC`, `CLAS` or `INTF`, and what it checks when it creates an object. It is not a substitute
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
- Name and text limits are checked before the create request is sent. See
  [Getting started](getting-started.md#other-source-objects). The matrix does not exercise them.

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
created and left behind stays in SAP until someone removes it. Deleting objects is not part of
this release.

## Transport requests and failed activation

For a transportable object, ABAPReach relies on the `CORRNR` returned by the
SAP lock. If the lock returns no request, it stops before syntax check or PUT,
even when `--tr` was supplied. It also refuses a mismatched `--tr`. See
[Behavior differences](behavior-differences.md#a-transportable-object-whose-lock-reports-no-transport-request-is-refused)
for the difference from the earlier Python tooling.

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

## Status and exit summary

| Result | Exit | Meaning |
| :--- | :---: | :--- |
| `ACTIVE`, `SIMULATED_OK`, `no changes` | 0 | done |
| `CREATE: ACTIVE`, `CREATE_SIM: SIMULATED_OK` | 0 | created, or simulated without a write |
| `INACTIVE`, `REJECTED`, `PARTIAL` | 1 | inspect the SAP state |
| `CREATE: REJECTED`, `CREATE: PARTIAL` | 1 | inspect the SAP state; read before retrying |
| `INCLUDE_MISSING: ...` | 1 | the class include does not exist; nothing written |
| `ACTIVE (reconciled)` | 1 | file is active; verify with `read` |
| `STOP: ...` | 2 | input, configuration, or guard |

Exit code 3 is not used by this release.

The final backend and guard report appears in deploy output. `read` reports
line count and hash, not a backend/guard report. `doctor` has its own exit
meaning: 0 only when all its probes pass, 1 otherwise.

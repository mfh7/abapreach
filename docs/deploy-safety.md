# Deploy safety

This guide describes what ABAPReach 0.12.0 checks before and after it writes
an existing `PROG`. It is not a substitute for reviewing your local change
and your SAP transport state.

## Recommended sequence

1. `read PROG` and keep the printed `hash`.
2. Review your local edits and the intended SAP target.
3. Run `deploy PROG ... --simulate --base <hash>`.
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

Do not automate a retry solely from the exit code. Re-read the program and
inspect its active and inactive state in SAP. If an inactive version remains,
decide how to restore or activate it deliberately. The tool does not perform
an automatic rollback.

## Status and exit summary

| Result | Exit | Meaning |
| :--- | :---: | :--- |
| `ACTIVE`, `SIMULATED_OK`, `no changes` | 0 | done |
| `INACTIVE`, `REJECTED`, `PARTIAL` | 1 | inspect the SAP state |
| `ACTIVE (reconciled)` | 1 | file is active; verify with `read` |
| `STOP: ...` | 2 | input, configuration, or guard |

The final backend and guard report appears in deploy output. `read` reports
line count and hash, not a backend/guard report. `doctor` has its own exit
meaning: 0 only when all its probes pass, 1 otherwise.

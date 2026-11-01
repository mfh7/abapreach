# abapreach

`abapreach` is a command line tool for ABAP source objects of an SAP system. Milestone 1
supports three commands for programs (`PROG`) over two backends:

- ADT over HTTP(S): the ABAP Development Tools REST API of the system.
- RFC/SADT: the same ADT requests tunnelled through the function module
  `SADT_REST_RFC_ENDPOINT`, for systems that are reachable by RFC or SAProuter only.

Both backends use the same operations, so a program behaves the same on either one. Some systems
keep no stateful ADT HTTP session (seen on a 740 release); `deploy` then falls back to RFC/SADT
and `doctor` reports it, while `read` is unaffected.

## Commands

    abapreach read PROG <NAME> <OUT_FILE> [--force] --conn <sid>_<client>
    abapreach deploy PROG <NAME> <FILE> [--simulate] [--base <hash>] [--tr <request>]
        [--backend adt|rfc] --conn <sid>_<client>
    abapreach doctor --conn <sid>_<client>

`--conn` is mandatory. `read` refuses to overwrite `OUT_FILE` without `--force` and prints the
line count and the content hash. `deploy` always prints a unified diff (`--- live` / `+++ file`)
and the `live hash`; `--simulate` runs everything up to a syntax check without saving and then
releases the lock. `--base` stops the run when the live source no longer has that hash. Both start
with a report line that names the backend, why it was chosen, the guards that hold on that path
(including the `foreign_inactive` policy) and the guards of the Python tooling that do not apply
there.

## Doctor

`doctor` is read-only and checks one connection per backend: the configuration and the
credentials are checked locally (their values are never printed), ADT gets one
`GET /sap/bc/adt/discovery`, the RFC library is looked up and asked for its `RfcGetVersion`,
and RFC gets one `RfcPing`, its only logon. Every probe is reported even when an earlier one
failed; a missing RFC library skips the ping instead of failing it. Exit 0 only when every
probe passed, 1 otherwise.

## Statuses and exit codes

| Result | Exit |
| --- | --- |
| `ACTIVE`, `SIMULATED_OK`, `no changes` | 0 |
| `INACTIVE`, `REJECTED`, `PARTIAL` | 1 |
| `STOP: ...` on stderr (bad argument, configuration, `--base`, changed source) | 2 |

After a write whose outcome is unknown (timeout or lost connection after the PUT or the
activation was sent) the tool only reads: it compares the active and the inactive source with
the file and reports `ACTIVE (reconciled)`, `INACTIVE`, `PARTIAL: outcome unknown`,
`PARTIAL: unexpected state` or `PARTIAL: reconciliation incomplete, inactive source unreadable
(<reason>)` when the inactive source cannot be read (HTTP status, timeout, bad UTF-8); every
result exits 1 with `check with read before retrying`. It never retries and never switches
backend.

## Configuration

The connections file is `$ABAP_CONNECTIONS` or `~/.config/abap-rfc/connections.json`; secrets
live in `connections.local.json` next to it. Per server: `systemId`, `host`, `port` (the ADT
endpoint), `clients`, `rfc` (`ashost`, `sysnr`, optional `saprouter`) and the optional `adt`
block (`scheme`, `ca_file`, `insecure`). The connection id is `<sid>_<client>`. User and
password come from `<SID>_<CLIENT>_USER` and `<SID>_<CLIENT>_PASS` in the environment, else
from `connections.local.json`. Passwords are never printed.

TLS is verified by default. `http://` and `insecure` exist only as per-connection settings in
the configuration file, never as command line flags, and every output and `doctor` then
prints a warning.

### Policies

`policies` is an optional object that holds the write guards of every server;
`servers[].policies` overrides single keys of it:

    {
      "policies": {"foreign_inactive": "reject"},
      "servers": [
        {"systemId": "DEV", "host": "example.com", "port": 443, "clients": ["100"],
         "rfc": {"ashost": "example.com", "sysnr": "00"},
         "policies": {"foreign_inactive": "ask"}}
      ]
    }

`foreign_inactive` decides what `deploy` does when the inactive version of the program belongs
to another user: before the lock, `deploy` (and `deploy --simulate`) reads `GET
<source>/versions` and compares the author of the inactive version (`99999`) with the logon
user, and it reads the same list again right after the lock, so a version another user saves in
between is still seen. ADT allows such an overwrite, the function module of the Python tooling
does not, so `abapreach` enforces it on both backends itself.

| Value | Behavior |
| --- | --- |
| `reject` (default) | `REJECTED: inactive version of another user exists: <OWNER>` and exit 1; no lock is sent |
| `allow` | writes anyway and prints `W: overwriting the inactive version of <OWNER> (policy allow)` |
| `ask` | asks `<NAME>: inactive version of <OWNER> would be overwritten (<n> lines changed vs active). Overwrite? [y/N]` on stderr; only `y`/`yes` continues, and without a terminal it behaves as `reject` (`(policy ask needs a terminal)`) |

An inactive version of the logon user is no obstacle. A version list that cannot be read or
parsed fails closed (`REJECTED`, exit 1) under `reject` and `ask`; only `allow` continues, with a
warning. A version that appears between the first check and the lock is refused after the lock
and the lock is released; a prompt is never shown while the lock is held, so `ask` then behaves
as `reject`. An unknown guard name or policy value stops the run at load time with exit 2.
`doctor` prints the effective policy of the connection.

## RFC dependency

The RFC backend loads the SAP NetWeaver RFC library at run time; nothing is linked or bundled.
It looks for `ABAPREACH_RFC_LIBRARY`, then the library of an installed SAP GUI (macOS), then
`SAPNWRFC_HOME` and the usual SDK folders. Without a library the RFC backend is unavailable and
ADT over HTTP is used.

## Limits of milestone 1

Only `PROG` objects that already exist; no object creation, no other object types, no MCP
server and no function-module fallback. The tool is built and tested on macOS arm64.

## Tests

Run `cargo test`; the fakes behind the `test-support` feature are enabled for tests by the
dev-dependency, so no extra flag is needed (`cargo test --features test-support` also works).

# Changelog

All notable changes to ABAPReach are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

**Current Rust CLI:** version `0.11.0` supports existing `PROG` objects only.
Versions `0.1.0` to `0.10.0` below describe the earlier Python implementation,
not features available in the current Rust CLI. Their tags live in the
internal repository where that implementation was developed. See the
[roadmap](docs/roadmap.md) for the path to 1.0.0, which brings all of them to Rust.

## [Unreleased]

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

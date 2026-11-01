# Changelog

All notable changes to ABAPReach are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/). Versions `0.1.0` to `0.10.0` are the earlier
Python implementation, which is the behavioural baseline of the Rust port; their tags live in
the internal repository where that implementation was developed.

## [Unreleased]

### Fixed
- A read-only GET without reply falls back like a connection failure.
- Doctor capability lines list only probed-ok backends.
- HTTP 401 is reported as `ERROR: authorization failed: HTTP 401`, exit 1, no fallback.
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

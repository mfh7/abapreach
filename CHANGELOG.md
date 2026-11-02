# Changelog

All notable changes to ABAPReach are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

**Current Rust CLI:** version `0.12.0` supports existing `PROG` objects only.
Versions `0.1.0` to `0.10.0` below describe the earlier Python implementation,
not features available in the current Rust CLI. Their tags live in the
internal repository where that implementation was developed. See the
[roadmap](docs/roadmap.md) for the path to 1.0.0, which brings all of them to Rust.

## [Unreleased]

### Fixed
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

# ABAPReach

**Let AI reach your ABAP code.**

ABAPReach is a Rust CLI for working with ABAP source in SAP systems—through direct ABAP Development Tools (ADT) HTTP(S) access or an RFC connection when HTTP access is unavailable. It gives developers, scripts, and external AI tooling a consistent way to inspect and change ABAP code, with explicit safeguards around write operations.

> **Early preview · v0.11.0 (Milestone 1)**  
> The current Rust release supports existing ABAP programs (`PROG`): read, deploy, simulate, and diagnostics. It is built and tested on **macOS arm64**. A native MCP server, additional ABAP object types, and custom function-module operations are **not implemented in this release**.

## Why ABAPReach?

ABAP development environments are not always reachable over HTTP(S). Some SAP landscapes are accessible only through RFC or SAProuter, making it difficult to use the same workflow across systems.

ABAPReach focuses on three things:

- **Reach the system you have.** Prefer ADT over HTTP(S); use RFC/SADT where the SAP system can be reached through RFC instead.
- **Use one consistent CLI.** Read, inspect changes, and deploy programs without maintaining separate scripts for each connection path.
- **Make writes deliberate.** Review diffs, check source hashes, respect inactive versions owned by other users, and avoid blind retries when a write's outcome is uncertain.

The CLI can be invoked by automation or an AI agent today. **Built-in MCP integration is not available yet.**

## Available today

| Capability | v0.11.0 |
| --- | --- |
| Read an existing ABAP program to a local file | Available |
| Deploy changes to an existing program | Available |
| Preview changes and run syntax checks without saving (`--simulate`) | Available |
| Protect against source drift (`--base`) | Available |
| Check configuration, connectivity, and RFC library (`doctor`) | Available |
| Direct ADT HTTP(S) and RFC/SADT connectivity | Available, subject to system prerequisites |
| Create ABAP objects or support object types beyond `PROG` | Not yet |
| Built-in MCP server or native custom-FM fallback | Not yet |

## Quick start

### 1. Build the CLI

You need a Rust toolchain with Rust 2024 edition support and access to an SAP system with appropriate credentials and authorizations.

```bash
git clone https://github.com/mfh7/abapreach.git
cd abapreach
cargo build --release
./target/release/abapreach --help
```

The repository is currently private; the clone command requires repository access. No public binary distribution is documented at this stage.

### 2. Configure a connection

Create `~/.config/abap-rfc/connections.json`:

```json
{
  "servers": [
    {
      "systemId": "DEV",
      "host": "sap.example.com",
      "port": 443,
      "clients": ["100"],
      "rfc": {
        "ashost": "sap.example.com",
        "sysnr": "00"
      }
    }
  ]
}
```

`host` and `port` describe the ADT HTTP(S) endpoint. The `rfc` section describes the RFC endpoint; add `saprouter` there when needed. For an **RFC-only** system, omit `host` and `port`. The current implementation still requires the `rfc` configuration section even when you intend to use ADT only.

The default connection file path is retained for compatibility with the earlier Python tooling. Set `ABAP_CONNECTIONS` to use another location. Credentials are loaded from environment variables first, or from `connections.local.json` beside the connection file.

For the example above, provide `DEV_100_USER` and `DEV_100_PASS` through your shell environment or a secret manager. **Do not put passwords in `connections.json` or commit secrets to Git.**

### 3. Check access and read a program

```bash
./target/release/abapreach doctor --conn DEV_100
./target/release/abapreach read PROG ZHELLO ./ZHELLO.abap --conn DEV_100
```

`read` refuses to overwrite an existing local file unless `--force` is supplied. It reports the line count and a content hash.

**Doctor caveat (v0.11.0):** `doctor` tests both configured connectivity paths and returns exit code 1 if *any* probe fails, including a missing RFC library. That does not necessarily mean ADT operations are unavailable. Check the individual probe results rather than treating its aggregate exit status as an ADT-only readiness verdict.

### 4. Preview, then deploy

```bash
./target/release/abapreach deploy PROG ZHELLO ./ZHELLO.abap --simulate --conn DEV_100

# Writes to SAP; run only after reviewing the diff and safety report.
./target/release/abapreach deploy PROG ZHELLO ./ZHELLO.abap --conn DEV_100
```

`--simulate` generates a diff and performs preflight checks, including syntax checking, without saving source changes. The tool acquires and releases locks when needed. For concurrency protection, supply `--base <hash>` using the live source hash from an earlier read or deploy report. Transportable objects may require `--tr <request>`, subject to the SAP-side transport and lock checks.

## Connectivity and dependencies

| Path | How it works | Requirements |
| --- | --- | --- |
| **ADT HTTP(S)** | Connects to the SAP ADT REST API | Reachable ADT endpoint; permitted user; stateful session for deploy |
| **RFC/SADT** | Sends ADT requests via `SADT_REST_RFC_ENDPOINT` over RFC | Reachable RFC endpoint (direct or SAProuter), compatible SAP RFC library, required SAP-side function module |

RFC/SADT is a transport for ADT semantics in this milestone, **not** a general-purpose native RFC feature backend. Support for an operation still depends on the target SAP system.

The RFC library is **loaded at runtime and is not included** in ABAPReach. The loader checks `ABAPREACH_RFC_LIBRARY`, supported local SAP GUI library locations (macOS), `SAPNWRFC_HOME`, and SDK directories. Installing or licensing the SAP RFC runtime remains the user's responsibility.

ABAPReach prefers direct ADT when configured. Automatic fallback to RFC is intentionally limited: it is allowed only where routing considers the alternative eligible and the previous attempt has not made an uncertain write. An explicit `--backend adt|rfc` or `--base` on deploy prevents automatic switching. Authorization, validation, and transport errors are not reasons to bypass safeguards by changing backends.

TLS verification is on by default for ADT. Insecure TLS or plain HTTP must be explicitly configured per connection and produces a warning.

## Safety by design

- **Review before write:** deploy shows a unified diff and reports the selected backend and applicable guards.
- **Drift protection:** `--base` checks that the live source still matches the expected hash, including after locking.
- **Inactive-version protection:** the default `foreign_inactive: reject` policy refuses to overwrite another user's inactive version. The optional `ask` policy requires an interactive terminal; `allow` explicitly relaxes the guard.
- **Controlled activation and transport:** syntax checks, locks, activation outcomes, and transport checks are part of deploy; a failed activation can leave an inactive version for inspection.
- **No blind retry:** when a write may have reached SAP but its outcome is unknown, ABAPReach reads back state for reconciliation. It never blindly retries the write or silently switches backend.

These safeguards do **not** replace SAP permissions, system configuration, change management, or a review of the actual source change.

### Exit codes

| Exit | Meaning |
| --- | --- |
| `0` | Successful operation, successful simulation, or no change |
| `1` | Rejected, inactive, partial, or unsuccessful operation; also used for a failed `doctor` probe |
| `2` | Argument, configuration, or failed precondition reported as `STOP` |

After a partial or uncertain write, inspect the current SAP state before attempting another deployment.

## Scope and documentation

**Milestone 1 is intentionally narrow.** It supports existing `PROG` objects on tested macOS arm64 setups. It does not yet create objects, cover other ABAP object types, expose an MCP server, or fall back to a custom deployment function module.

For implementation details and known differences from the previous Python tooling, see:

- [Behavior differences](docs/behavior-differences.md)
- [Changelog](CHANGELOG.md)

To run the test suite:

```bash
cargo test
```

ABAPReach is under active development. Availability of a command in an earlier Python implementation does not imply that it is present in the current Rust binary.

---

*ABAP and SAP are trademarks of SAP SE or its affiliates. ABAPReach is an independent project and is not presented as an SAP product.*

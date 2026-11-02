# Getting started

ABAPReach 0.12.0 is a macOS arm64-tested CLI for reading and deploying the active
source of **existing ABAP programs** (`PROG`). Release 0.12.0 targets Windows x64,
Linux x64 and Linux arm64 as well; see [Platforms and release binaries](#platforms-and-release-binaries)
and the [0.12.0 test matrix](#tested-environment) for what is verified so far. You need a Rust toolchain that
supports edition 2024, an SAP user allowed to access the relevant ADT or RFC
services, and a reachable SAP system. Deploy also needs the SAP-side permissions
and transport state required for the target program. Check those with your SAP
administrator if a probe or deploy is refused.

## Build from source

From the repository root:

```sh
cargo build --release
./target/release/abapreach --version
./target/release/abapreach --help
```

The binary is `target/release/abapreach`. No SAP library is linked or bundled at
build time. The RFC backend needs a compatible SAP NetWeaver RFC library at run
time; ADT HTTP does not.

## Platforms and release binaries

The release targets and the paths each one offers. Test status for each cell is in
[Tested environment](#tested-environment).

| Target | ADT HTTP | RFC, SAP GUI library | RFC, NW RFC SDK |
| :--- | :--- | :--- | :--- |
| macOS arm64 | yes | yes (SAP GUI for Java) | yes |
| Windows x64 | yes | yes (SAP GUI for Windows, 64-bit) | yes |
| Linux x64 | yes | yes (SAP GUI for Java) | yes |
| Linux arm64 | yes | not available: SAP GUI for Java for Linux is built for x86_64 only | yes |

The Windows binary is x64. Windows on ARM runs it under x64 emulation together with
SAP GUI.

### Release files

Each release lists one archive per target, named `abapreach-<version>-<target>`:
a `.zip` for Windows and a `.tar.gz` for macOS and Linux. The archive holds the single
executable (`abapreach`, or `abapreach.exe` on Windows). `<target>` is one of
`aarch64-apple-darwin` (macOS arm64), `x86_64-pc-windows-msvc` (Windows x64),
`x86_64-unknown-linux-gnu` (Linux x64) and `aarch64-unknown-linux-gnu` (Linux arm64).
Each archive has a SHA256 value on the release page and in a `.sha256` file next to
it. Check it before you run the binary:

```sh
shasum -a 256 abapreach-<version>-<target>.tar.gz
```

```sh
sha256sum abapreach-<version>-<target>.tar.gz
```

```text
certutil -hashfile abapreach-<version>-<target>.zip SHA256
```

- **Windows x64:** the binary links the C runtime statically, so no Visual C++
  redistributable is needed. The binary is not code-signed yet, so Windows
  SmartScreen can block or warn on the first start. Run it only from a release you
  checked against the SHA256 value.
- **macOS:** a file downloaded from a browser carries the quarantine attribute, which
  Gatekeeper checks. If the binary is blocked, remove the attribute:

  ```sh
  xattr -d com.apple.quarantine /path/to/abapreach
  ```

- **Linux:** the release binary needs glibc 2.28 or newer. It is built against glibc
  2.28. musl-based systems are not supported, because the SAP libraries need glibc.
  The `.tar.gz` keeps the executable bit. If the file is not executable after
  extraction, run `chmod +x /path/to/abapreach`. On a headless system, install the
  `ca-certificates` package; without it HTTPS connections fail certificate checks
  (see [TLS settings](#tls-settings)).

### RFC library lookup

Without an override, the loader tries these candidates in order and uses the first one
whose architecture matches the binary and that loads: SAP GUI library first, then the
NW RFC SDK library. A discovered candidate that fails moves on to the next one.

| Target | SAP GUI library (family `SAP GUI`) | NW RFC SDK library (family `NW RFC SDK`) |
| :--- | :--- | :--- |
| Windows x64 | `%ProgramFiles%\SAP\FrontEnd\SAPGUI\sapnwrfccm.dll` (64-bit install only) | `sapnwrfc.dll` in `lib` under `%SAPNWRFC_HOME%`, `%USERPROFILE%\sap\nwrfcsdk`, then `%ProgramFiles%\SAP\nwrfcsdk`; this SDK DLL needs the Microsoft Visual C++ 2015-2022 x64 Redistributable because it imports `MSVCP140.dll` and `VCRUNTIME140.dll` (the `abapreach.exe` binary itself does not) |
| macOS arm64 | `bin/unicode/libsapnwrfc.dylib`, then `bin/libsapnwrfccm.dylib`, inside each SAP GUI app bundle under `/Applications` (searched to four levels; the bundle with the highest version in its name is tried first) | `libsapnwrfc.dylib` in `lib` under `$SAPNWRFC_HOME`, `$HOME/sap/nwrfcsdk`, `/opt/sap/nwrfcsdk`, then `/usr/local/sap/nwrfcsdk` |
| Linux x64 | `bin/unicode/libsapnwrfc.so`, then `bin/libsapnwrfccm.so`, inside `$HOME/sapgui`, `/opt/sapgui`, then `/usr/local/sapgui` | `libsapnwrfc.so` in `lib` under `$SAPNWRFC_HOME`, `$HOME/sap/nwrfcsdk`, `/opt/sap/nwrfcsdk`, then `/usr/local/sap/nwrfcsdk` |
| Linux arm64 | Unsupported: SAP GUI for Java Linux is x86_64 only | `libsapnwrfc.so` in `lib` under `$SAPNWRFC_HOME`, `$HOME/sap/nwrfcsdk`, `/opt/sap/nwrfcsdk`, then `/usr/local/sap/nwrfcsdk` |

On Windows, a 32-bit SAP GUI under `Program Files (x86)` is skipped, because a 32-bit
library cannot load into this 64-bit binary; `doctor` prints the reason.

An explicit path always wins: set `ABAPREACH_RFC_LIBRARY` to the library file.
If that path cannot be loaded, ABAPReach stops and reports it. It does not try the
discovered candidates:

```text
could not load RFC library <path>: <reason>; an explicit path does not fall back to another candidate
```

When no candidate exists at all, the message is `RFC library not found; set ABAPREACH_RFC_LIBRARY`.
A library built for another architecture is refused before it is loaded, with
`architecture mismatch: the library is <found> but this binary is <expected>`, where
the labels are `x86-64`, `arm64`, `x86 (32-bit)` and `arm (32-bit)`.

On success, `doctor` prints the loaded library:

```text
RFC library: <path> version <version> architecture <architecture> family <SAP GUI or NW RFC SDK> source <explicit ABAPREACH_RFC_LIBRARY or discovered> chars <UTF-16 or single-byte>
```

On Linux, if an RFC library does not declare `$ORIGIN` in `RUNPATH` or `RPATH`, ABAPReach looks
in that library's own folder for one complete ICU set and maps it by absolute path in this order:
`libicudata*.so*`, `libicuuc*.so*`, then `libicui18n*.so*`. If several complete sets exist, it
uses the highest version. This is required for the NW RFC SDK, which opens ICU by bare SONAME at
run time; ABAPReach does not preload `libsapucum.so`. If no complete set is found, the candidate
still loads and doctor prints exactly `WARNING: RFC ICU libraries were not found beside the RFC
library; the NW RFC SDK may still find them through LD_LIBRARY_PATH.` Keep the SDK folder intact,
or set `LD_LIBRARY_PATH` to that folder so the SDK can find ICU itself. The parent process's
`LD_LIBRARY_PATH` is not changed.

On Windows, the loader searches the folder of the library for its static dependencies, but the NW
RFC SDK opens its ICU libraries later by bare name, and that folder is not on the search path. So
ABAPReach maps `icudt<N>.dll`, `icuuc<N>.dll`, then `icuin<N>.dll` (highest complete version, names
matched ignoring case) from the SDK folder by absolute path before it maps `sapnwrfc.dll`; a later
load by bare name finds the module that is already mapped. No `PATH` entry is needed. If no complete
set is found next to `sapnwrfc.dll`, the library still loads and doctor prints `WARNING: RFC ICU
libraries were not found beside the RFC library; the NW RFC SDK may still find them through PATH.`
Only a library named `sapnwrfc.dll` gets this treatment (and this line): the SAP GUI library
`sapnwrfccm.dll` needs no ICU, so nothing is mapped for it and it gets no such line.

The SAP GUI 8.00 for Windows library is a single-byte build: it reads every string as `char*`
instead of UTF-16. ABAPReach reads the width from the version text the library returns, passes
strings in that width, and prints it as `chars UTF-16` or `chars single-byte` on the library line.
Such a library accepts ASCII only in the request metadata (the URI, the headers, and the
connection parameters such as the password): a non-ASCII character there stops with `a single-byte
RFC library accepts ASCII only; the text contains other characters; use the NW RFC SDK library
(ABAPREACH_RFC_LIBRARY=...\sapnwrfc.dll) instead`. ABAP source is not affected: it travels as
XSTRING bytes, so non-ASCII source works through the single-byte library. The SDK library has no
limit on the metadata either.

SAP GUI for Java libraries declare `$ORIGIN`, so they are loaded without a preload step.

### Local credentials file permission

`connections.local.json` holds passwords. Keep it readable by your user only:

- **Unix and macOS:** no group or other permission bits, for example `chmod 600`.
  `doctor` prints `local credentials: user-only permission` for such a file. If the file
  is readable by group or other, `doctor` prints a `WARNING: local credentials are
  readable by group or other (mode ...)` line. The warning does not stop the command.
- **Windows:** `doctor` prints `local credentials: permission not checked; restrict the file
  to a user-only ACL`. Remove access for other accounts in the file's security settings
  yourself; ABAPReach does not check the ACL.

A missing file is reported as `local credentials: not present`.
If its metadata cannot be read on Unix, doctor prints a `WARNING: local credentials metadata
could not be read: ...` line. This is non-blocking and does not change doctor's exit status.

### Trace files and interrupts

The SAP library writes its trace file `dev_rfc.log` into the current directory unless
told otherwise. ABAPReach points it at the OS temporary directory: it sets `RFC_TRACE_DIR`
at startup, before any other thread exists, and calls `RfcSetTraceDir` when the library
exports it. On Windows it also writes the variable into the environment of the shared UCRT
(`ucrtbase.dll`): the SAP libraries read it through `getenv`, which does not see a variable set
only through the Win32 API, and a statically linked executable never fills that copy at start-up.
So the library's first call already logs to the temporary directory. A library without
`RfcSetTraceDir` relies on the environment variable alone. `doctor` records the outcome on the
line after the library line, one of:

```text
RFC trace directory: RfcSetTraceDir exported, set
RFC trace directory: RfcSetTraceDir not exported, environment only
RFC trace directory: RfcSetTraceDir exported, returned rc <rc>, environment only
RFC trace directory: RfcSetTraceDir exported, not called (the temp directory is not ASCII and the library uses single-byte characters), environment only
```

On Ctrl-C, ABAPReach exits with code 130 when no deploy lock is held. When a deploy holds
the lock, the interrupt is deferred: the deploy finishes its lock release, prints
`CANCELED: interrupted while the lock was held` (or `CANCELED: interrupted before the
activation` when the new source was already saved inactive), and exits with code 130. The
exit code is 130 only after the lock is released; if the release fails, the exit code is 1 and
the output says the lock may remain. On Unix, SIGTERM
and SIGHUP (for example a dropped SSH session) take the same path. On
Windows, CTRL_BREAK does too, but for CTRL_CLOSE, CTRL_LOGOFF and CTRL_SHUTDOWN the OS ends
the process as soon as the handler returns, so a lock may stay held in SAP.

## Configure a connection

By default, ABAPReach reads `~/.config/abap-rfc/connections.json`. Set
`ABAP_CONNECTIONS` to use another path. A server needs an `rfc` block even when
you intend to use ADT HTTP; omit `host` and `port` together for an RFC-only
connection.

The connection ID names one client of one system: `<SID>_<client>`.

```text
A4H_001
 |   |
 |   +-- client: one of servers[].clients
 +------ SID: servers[].systemId, in upper case
```

`--conn` accepts the SID in either case; `systemId` in the file must be upper case.

Example `connections.json` for an ADT HTTP and RFC-capable system:

```json
{
  "servers": [
    {
      "systemId": "A4H",
      "host": "sap.example.com",
      "port": 443,
      "clients": ["001"],
      "rfc": {"ashost": "sap.example.com", "sysnr": "00"}
    }
  ]
}
```

For an RFC-only system, use the same server entry without `host` and `port`.

### Connect through SAProuter

Many systems are reachable only through a SAProuter. Keep the application
server in `ashost` as the router sees it, and give the route once per system.

A route without a password may stay in the server entry as `saprouter` inside
`rfc`:

```json
{
  "servers": [
    {
      "systemId": "A4H",
      "clients": ["001"],
      "rfc": {
        "ashost": "sap.example.com",
        "sysnr": "00",
        "saprouter": "/H/saprouter.example.com/S/3299"
      }
    }
  ]
}
```

| Part | Meaning |
| :--- | :--- |
| `/H/<host>` | SAProuter host |
| `/S/<port>` | SAProuter port, usually `3299` |
| `/W/<password>` | route password, only if the router asks for one |

For a chain of routers, repeat `/H/.../S/...` for each hop. Your SAP Basis team
or the SAP Logon entry of the system (connection string) gives the exact route.
ABAPReach passes the string unchanged to the RFC library as `SAPROUTER`.

A route with `/W/` carries a password. Put such a route in
`connections.local.json` (or the environment) under `<SID>_SAPROUTER` instead
of in `connections.json`, so the password stays in the file you already keep
out of version control:

```json
{
  "A4H_001_USER": "your-user",
  "A4H_001_PASS": "your-password",
  "A4H_SAPROUTER": "/H/saprouter.example.com/S/3299/W/route-password"
}
```

The key is per system, not per client: it has no client part, and one route
serves every client of that system. Lookup follows the credentials keys, so an
`A4H_SAPROUTER` environment variable wins over `connections.local.json`, which
wins over `servers[].rfc.saprouter` in `connections.json`. A route set there
replaces the config value for every client of the system; when neither holds a
route, no `SAPROUTER` parameter is passed.

ADT HTTP does not travel through a SAProuter, so this example has no `host`
and `port`: the connection runs on RFC only. If the system is also reachable
over HTTPS, add `host` and `port` and both backends are available.

ABAPReach never prints the route, because it may contain a route password.
`doctor` reports only where the route came from, and the route is scrubbed from
command output like a password:

```text
SAProuter: configured (from connections.local.json)
```

Credentials use the connection ID with `_USER` and `_PASS`. Provide
`A4H_001_USER` and `A4H_001_PASS` in the environment, or put a flat
JSON object named `connections.local.json` next to `connections.json`:

```json
{
  "A4H_001_USER": "your-user",
  "A4H_001_PASS": "your-password"
}
```

Keep the local credentials file out of version control and limit its file
permissions; the per-OS rules are in
[Local credentials file permission](#local-credentials-file-permission). Environment
variables take precedence over values in the file.
The connection file and the local credentials file must be readable by the
process. ABAPReach does not print passwords.

For a custom connections file:

```sh
export ABAP_CONNECTIONS=/path/to/connections.json
```

### TLS settings

HTTPS certificate verification is on by default. Certificates are checked against the
operating system's trust store. On a headless Linux system, install `ca-certificates`
first.

For a custom CA, add an `adt` block to the server entry:

```text
"adt": {"scheme": "https", "ca_file": "/path/to/ca.pem"}
```

`ca_file` is a PEM file; its certificates are added as trusted roots for that
connection, on top of the operating system's trust store, not in place of it. ABAPReach stops with `cannot read CA file <path>: <reason>` when the file
cannot be read, and with `no certificate found in CA file <path>` when it holds no
certificate.

The `adt` block also accepts `"insecure": true` or `"scheme": "http"` as
per-connection settings. Those modes print a warning on command output and in
`doctor`; there are no command-line flags for them.

## Check the connection

```sh
./target/release/abapreach doctor --conn A4H_001
```

`doctor` checks configuration and credential availability locally, then probes
ADT HTTP discovery and the RFC library and ping. It makes no ABAP object
changes, and it logs on at most once per backend, so a wrong password cannot
lock the SAP user through repeated attempts. It reports each probe even if another probe failed. An RFC library
failure skips the RFC ping, **but the overall exit code is still 1**. If ADT
HTTP passes and RFC fails, ADT reads may still work; inspect the probe lines
instead of treating exit 1 as proof that every backend is unusable.

For RFC, set `ABAPREACH_RFC_LIBRARY` to the library path if automatic lookup
does not find it. The lookup order per OS, and what happens when an explicit path
fails, are in [RFC library lookup](#rfc-library-lookup). `doctor` also reports
whether ADT HTTP has a stateful session, which deploy needs for its lock.

## Read an existing program

```sh
./target/release/abapreach read PROG ZDEMO ./ZDEMO.abap --conn A4H_001
```

`read` writes the active source, prints its line count and a `hash` value, and
refuses to overwrite an existing output file unless you add `--force`. Keep
that hash for a later `--base` check. The hash is ABAPReach's normalized
content hash; do not substitute a hash produced by an unrelated file utility.

The successful output has this shape (values below are illustrative):

```text
42 lines -> ./ZDEMO.abap
hash 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

## Simulate, then deploy

Edit a copy of the file you read. Review the change locally before sending
it to SAP. Replace `HASH_FROM_READ` with the exact value printed by `read`:

```sh
./target/release/abapreach deploy PROG ZDEMO ./ZDEMO.abap --simulate --base HASH_FROM_READ --conn A4H_001
./target/release/abapreach deploy PROG ZDEMO ./ZDEMO.abap --base HASH_FROM_READ --conn A4H_001
```

Simulation checks syntax without saving and releases the lock. If the live
source has changed, `--base` stops the run. If `doctor` reports that ADT HTTP
has no stateful session, add `--backend rfc` to both commands after checking
the RFC probe; `--base` prevents automatic backend fallback. A transportable
object may require `--tr <request>`; the request must agree with the SAP lock
response. Deploy can leave an inactive version after failed activation. Read
[Deploy safety](deploy-safety.md) and [Behavior differences](behavior-differences.md)
before the first write.

ABAPReach 0.12.0 has no MCP server yet; it is part of 1.0.0 (see the
[roadmap](roadmap.md#mcp-server)). An AI tool may invoke the CLI as part of a
user-controlled workflow, but the CLI does not grant the AI any SAP
permissions of its own.

## Tested environment

Release 0.12.0 test matrix. Every cell is `pending`: no run has been recorded yet. Each
cell becomes PASS, UNSUPPORTED (with the reason) or FAIL after the release-candidate
run on the released artifacts. A FAIL on a supported path blocks the release.

| Target | Host | ADT HTTP | RFC, SAP GUI library | RFC, NW RFC SDK |
| :--- | :--- | :--- | :--- | :--- |
| macOS arm64 | arm64 workstation (regression of 0.11.1) | pending | pending | pending |
| Windows x64 | x64 binary on an arm64 VM, under emulation; timings labelled `emulated` | pending | pending | pending |
| Linux x64 | x86_64 host | pending | pending | pending |
| Linux arm64 | arm64 host | pending | not available: SAP GUI for Java for Linux is built for x86_64 only | pending |

Per cell, the scenarios are `doctor`, three reads, `deploy --simulate`, `deploy`, an
unchanged deploy, a base hash mismatch, a syntax error, a wrong password, a reset, a
foreign inactive version under `reject` and `allow`, `ask`, `--tr`, and RFC fault
injection with the separate fault-injection artifact. Linux and Windows cells also cover
the loader, trace, source-encoding, credential-permission and CA-trust checks described
above. Each result records the loaded library path, version and architecture.

The Linux x64 SAP GUI cell stays `pending` until a run on a Linux x64 host records the
library that loads. The library layout in [RFC library lookup](#rfc-library-lookup) comes
from the SAP GUI for Java install package, not from a run.

### 0.11.1 record (macOS arm64)

ABAPReach 0.11.1 was tested live on three paths: ADT HTTP, RFC with the
library from SAP GUI, and RFC with the NW RFC SDK. Each path ran `doctor`,
`read`, `deploy --simulate`, `deploy`, an unchanged deploy, a base hash
mismatch, a syntax error, a wrong password, and (on 7.50) another user's
inactive version under the `reject` and `allow` policies.

### Workstation

| Component | Version |
| :--- | :--- |
| Operating system | macOS 27.0.1, arm64 |
| SAP GUI for Java | 8.10 revision 15 (bundle version `810.4.1500`), macOS aarch64 |
| RFC library from SAP GUI | `libsapnwrfc.dylib` 7.53 patch level 1629, with ICU 50 |
| NW RFC SDK | `libsapnwrfc.dylib` 7.50 patch level 20, with ICU 57 |
| Rust | 1.99.1 |

ABAPReach uses the SAP GUI library when it finds one. Point
`ABAPREACH_RFC_LIBRARY` at the SDK library to use the SDK instead. `doctor`
prints the path and version of the library it loaded.

### SAP systems

| SAP_BASIS | SAP_ABA | ADT HTTP | RFC, SAP GUI library | RFC, NW RFC SDK |
| :--- | :--- | :--- | :--- | :--- |
| 7.50 SP 13 | 7.50 SP 13 | all scenarios | all scenarios | all scenarios |
| 7.40 SP 17 | 7.40 SP 17 | read only ¹ | all scenarios ² | all scenarios ² |

¹ This system keeps no stateful ADT HTTP session. A deploy with `--base` or
`--backend adt` stops before locking; a deploy without them detects this and
runs on RFC.
² Without the inactive-version scenario: no second test user on that system.

Both RFC paths reached the systems through SAProuter.

### Timing

Median wall time in milliseconds, one workstation, the same network, the same
program. Each RFC command opens its own RFC connection through SAProuter.
`doctor` on RFC, which only opens a connection and pings, takes 450 to 500 ms,
so most of the gap to ADT HTTP is that connection setup.

| Command | 7.50 ADT HTTP | 7.50 RFC GUI | 7.50 RFC SDK | 7.40 RFC GUI | 7.40 RFC SDK |
| :--- | ---: | ---: | ---: | ---: | ---: |
| `read` | 337 | 737 | 692 | 676 | 645 |
| `deploy --simulate` | 737 | 975 | 923 | 903 | 803 |
| `deploy` | 1015 | 1179 | 1209 | 1094 | 975 |
| unchanged `deploy` | 323 | 723 | 744 | 680 | 607 |

`read` is the median of three runs; the other rows are single runs. A second
measurement of 10 `read` runs of a 6-line program on 7.40 gave 147 ms over ADT
HTTP (132 to 162), 703 ms over RFC with SAP GUI (645 to 716), and 670 ms over
RFC with the SDK (598 to 720). The two RFC libraries stay within each other's
range.

The same matrix passed with SAP GUI 8.10 revision 11 (RFC library patch level
1544) before the upgrade to revision 15. The timing table comes from the
revision 11 run.

### Also tested on the RFC paths

- A lost reply after a write, with the reply dropped by a test-only build
  (`--features fault-injection`, `ABAPREACH_FAULT=drop-reply:PUT|ACTIVATE`): a
  dropped PUT reply reports `INACTIVE`, a dropped activation reply reports
  `ACTIVE (reconciled)`, both exit 1, on 7.40 and 7.50 with both RFC
  libraries. The ADT HTTP path was tested the same way through a local proxy.
- The `ask` prompt: `n` refuses (`REJECTED`, exit 1), `y` overwrites, with
  both RFC libraries.
- A deploy into a transportable package: the request comes from the lock,
  `--tr` with that request deploys, a different `--tr` stops before the PUT,
  with both RFC libraries.

### Not tested yet

- Windows and Linux: no result is recorded yet; see the 0.12.0 matrix above.
  SAP GUI for Java ships per platform, so the Linux package and the RFC
  library inside it are checked separately; the macOS result does not carry over.
- The `ask` prompt and a transportable deploy over ADT HTTP on 7.50: the ADT
  HTTP endpoint of that system was unreachable during these runs.
- `libsapnwrfccm.dylib`, a second RFC library that SAP GUI 8.10 revision 15
  ships next to `libsapnwrfc.dylib`; ABAPReach loads `libsapnwrfc.dylib`.

macOS on Intel is not supported: SAP GUI for Java 8.10 is the last release for
Intel Macs.

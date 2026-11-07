# Getting started

ABAPReach is a CLI for reading and deploying the active source of **ABAP programs** (`PROG`),
function modules (`FUNC`), classes (`CLAS`) and interfaces (`INTF`), and for creating function
groups, function modules, classes and interfaces. It runs on macOS arm64, Windows x64, Linux x64
and Linux arm64; see [Platforms and release binaries](#platforms-and-release-binaries) and the
[Tested environment](#tested-environment) for what was tested. You need a Rust toolchain that
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
| Linux x64 | `bin/unicode/libsapnwrfc.so`, then `bin/libsapnwrfccm.so`, inside each root `$HOME/sapgui`, `/opt/sapgui`, then `/usr/local/sapgui`: first the root itself, then each versioned install folder directly below it (for example `SAPGUI8.10rev14`; the highest version, revision included, is tried first; a launcher-only `SAPGUI` folder is ignored) | `libsapnwrfc.so` in `lib` under `$SAPNWRFC_HOME`, `$HOME/sap/nwrfcsdk`, `/opt/sap/nwrfcsdk`, then `/usr/local/sap/nwrfcsdk` |
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
the output says the lock may remain. An interrupt that arrives after the lock was released
and the activation has started does not stop the activation: the command reports the real
result (for example `ACTIVE`, exit code 0). On Unix, SIGTERM
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

ABAPReach also runs as an MCP server; see [MCP server](mcp.md). An AI tool may invoke the CLI
as part of a user-controlled workflow, but the CLI does not grant the AI any SAP
permissions of its own.

## Other source objects

Besides programs, ABAPReach reads and deploys function modules (`FUNC`), classes with their
local includes (`CLAS`), and interfaces (`INTF`). It also creates function groups (`FUGR`),
`FUNC`, `CLAS` and `INTF` objects, and programs and includes (`PROG`);
see [Program includes](#program-includes). The commands take the same guards and status tokens as
`PROG` (see [Deploy safety](deploy-safety.md)).

```sh
./target/release/abapreach read FUNC ZDEMO_FM - --lines 1-20 --conn A4H_001
./target/release/abapreach read CLAS ZCL_DEMO ./ZCL_DEMO.abap --conn A4H_001
./target/release/abapreach read CLAS ZCL_DEMO ./ZCL_DEMO_CCIMP.abap --include CCIMP --conn A4H_001
./target/release/abapreach read INTF ZIF_DEMO ./ZIF_DEMO.abap --conn A4H_001
```

`read --lines A-B` prints the inclusive line range `A` to `B` (one-based) to stdout. The
output file must be `-`; any other value stops with exit 2. ABAPReach reads and hashes the
whole source first, so the `live hash` line on stderr is the hash of the full source, not of
the range. The matrix exercises `--lines` on `FUNC` only.

```sh
./target/release/abapreach deploy FUNC ZDEMO_FM ./ZDEMO_FM.abap --simulate --base HASH_FROM_READ --conn A4H_001
./target/release/abapreach deploy CLAS ZCL_DEMO ./ZCL_DEMO_CCIMP.abap --include CCIMP --simulate --base HASH_FROM_READ --conn A4H_001
./target/release/abapreach deploy INTF ZIF_DEMO ./ZIF_DEMO.abap --simulate --base HASH_FROM_READ --conn A4H_001
```

`--include` is accepted only for `CLAS`: `CCIMP`, `CCDEF`, `CCMAC` or `CCAU` (test classes).
Without `--include`, `CLAS` means the main source.

The file format of a `FUNC` is the ADT format: `FUNCTION <name> IMPORTING ... .` followed by
the body. A file in the Python format, whose first line is the `*"` interface header, is
refused with exit 2 (`STOP`). An unchanged signature is not reported; a changed
signature prints `INTERFACE CHANGED`, the parameter diff and the callers (see
[Deploy safety](deploy-safety.md#function-modules)).

A `CLAS` local include that does not exist yet stops the deploy with
`INCLUDE_MISSING: <kind> (re-run with --create-include)` and exit 1, before any write. Add
`--create-include` to create it. The create runs once, in the same class lock, before the
source PUT. The flag is only valid with `CLAS --include`.

```sh
./target/release/abapreach deploy CLAS ZCL_DEMO ./ZCL_DEMO_TESTS.abap --include CCAU --create-include --simulate --base HASH_FROM_READ --conn A4H_001
```

Create takes a name, a text and a package, and creates an empty object (a skeleton); the source
follows with `deploy`:

```sh
./target/release/abapreach create FUGR ZDEMO_FG --text 'Demo function group' --package ZDEMO --simulate --conn A4H_001
./target/release/abapreach create FUNC ZDEMO_FM --fugr ZDEMO_FG --text 'Demo function module' --simulate --conn A4H_001
./target/release/abapreach create CLAS ZCL_DEMO --package ZDEMO --text 'Demo class' --conn A4H_001
```

Create accepts names that start with `Z` or `Y` only. Limits, checked before the create
request: names up to 40 characters for `PROG`, 26 for `FUGR` and 30 for `FUNC`, `CLAS`
and `INTF`; texts up to 70 for `PROG`, 40 for `FUGR`, 74 for `FUNC` and 60 for `CLAS`
and `INTF`. A `FUNC` is created inside an existing
function group named with `--fugr`; `--package` is not valid for `create FUNC`. `--tr` is refused
for a `$TMP` package, and a transportable package requires `--tr`. The live matrix does not
exercise the name and text limits.

## Program includes

`create PROG` makes a program (`--subc 1`, the default) or an include (`--subc I`). `--package`
defaults to `$TMP`; a transportable package needs `--tr`.

```sh
./target/release/abapreach create PROG ZEXAMPLE --text 'Demo program' --simulate --conn A4H_001
./target/release/abapreach create PROG ZEXAMPLE --text 'Demo program' --conn A4H_001
./target/release/abapreach create PROG ZEXAMPLE_INC --subc I --text 'Demo include' --conn A4H_001
```

An include is read, deployed and deleted under its own name, as `PROG`. Its checks and its activation
need a main program as context. Give it with `--main`, or let ABAPReach use the one program that the
include's main program list names:

```sh
./target/release/abapreach read PROG ZEXAMPLE_INC ./ZEXAMPLE_INC.abap --conn A4H_001
./target/release/abapreach deploy PROG ZEXAMPLE_INC ./ZEXAMPLE_INC.abap --main ZEXAMPLE --simulate --base HASH_FROM_READ --conn A4H_001
./target/release/abapreach delete --simulate PROG:ZEXAMPLE_INC --plan delete.plan --conn A4H_001
```

- `--main` is only valid for a `PROG` include. On a program it stops with exit 2. Read needs no `--main`.
- Deploy and simulate print `main <PROG>` with the program that was used. If the list names several
  programs and no `--main` is given, the command stops with exit 2 and names them.
- A new include has no main program list yet, so its first simulate cannot run a syntax check. It
  prints `W: no syntax check before the first activation of a new include (main <PROG>)`; the
  contextual activation of the deploy is then the check. On the matrix systems this state did not
  arise live, so the warning path is covered by offline fixtures only.
- A `delete` of a program deletes it before the includes of the same batch. SAP refuses to delete an
  include that a program still references.

`create` with a FILE is accepted only for `PROG --subc 1`. It runs a simulate deploy after
`CREATE: ACTIVE` and writes no source; the source is deployed afterwards with `deploy`. `create PROG
--subc I` takes no FILE: deploy the include source after the `INCLUDE` statement exists in the parent
program.

## Read a table and delete objects

`read-table` reads rows of one table through the ADT data preview:

```text
abapreach read-table T000 --fields MANDT,MTEXT --rows 3 --conn A4H_001
```

`--rows` takes 1 to 5000 (default 100). `--where` takes a condition with fields, comparison
operators, `AND`, `OR`, `NOT`, parentheses and literals. Tables on the read-table deny list are
refused; see [MCP server: Trust boundaries](mcp.md#trust-boundaries).

`delete` removes objects only from a plan written by `--simulate`:

```text
abapreach delete --simulate PROG:ZEXAMPLE --plan delete.plan --conn A4H_001
abapreach delete PROG:ZEXAMPLE --plan delete.plan --conn A4H_001
```

Add `--tr TRKORR` when the object is transportable. Delete has a reconciliation and recovery
path; see [Deploy safety: Delete](deploy-safety.md#delete).

To use ABAPReach from an AI assistant, see [MCP server](mcp.md).

## Tested environment

The 0.15.0 release matrix ran on `v0.15.0-rc.6` (the binaries that were released as 0.15.0), with the release and the fault-injection builds,
on each platform and path, against two systems. It covers the core, platform, fault-injection, ca,
delete, read-table, policy, MCP and Python-parity groups, and the 0.15.0 scenarios (program create
and include, include delete, Python parity, the MCP write tools).
The interactive group (prompts and Ctrl-C) is from the 0.13.0 matrix (`v0.13.0-rc.4`) and was not
rerun for 0.15.0. The two systems:

- a SAP 7.50 system with a stateful ADT HTTP session, a second developer user and transport
  requests;
- a SAP 7.40 system with local objects only (`$TMP`), no stateful ADT HTTP session.

Each cell has one of three results:

- **PASS**: the scenario ran on that platform and path and its checks held.
- **UNSUPPORTED**: the scenario could not run there, for the reason the cell gives. It is not
  a failure. It is also not evidence that the feature works in that case.
- **FAIL**: the scenario ran on a supported path and a check did not hold. No cell failed in the
  0.15.0 matrix on its final run (see the note under the table).

### Support by platform and path

Each row is one platform. Each cell shows PASS / UNSUPPORTED / FAIL counts over the matrix cells of
that platform on that system.

| Platform | SAP 7.50 system | SAP 7.40 system |
| :--- | :---: | :---: |
| macOS arm64 | 456 / 108 / 0 ¹ | 333 / 231 / 0 ¹ |
| Windows x64, native | 456 / 108 / 0 ¹ | 333 / 231 / 0 ¹ |
| Windows x64, emulated on an ARM64 virtual machine | 456 / 108 / 0 | 333 / 231 / 0 ¹ |
| Linux x64 | 458 / 108 / 0 ¹ | 335 / 231 / 0 |
| Linux arm64 | 304 / 262 / 0 | 199 / 367 / 0 ¹ |

¹ The MCP write group was rerun on the same host and binary. In the first run, the plan-token
binding cell of the test harness sent an argument that the 0.15.0 MCP schema does not have; the
server ignored it, as it ignores every unknown argument, and the cell failed on its own wrong
expectation. The harness was fixed and the group was rerun: every cell passed. The count above is
the first run with the rerun group in place of its first result.

Totals: the SAP 7.50 system has 2130 PASS, 694 UNSUPPORTED and 0 FAIL cells. The SAP 7.40 system has
1533 PASS, 1291 UNSUPPORTED and 0 FAIL cells. Every lane reported no leftover test objects, and the
sweeps at start and end passed on every final run, each verified in a fresh session.

The MCP cells drive the server with a scripted client that reproduces each client's advertised
capabilities. The real clients were tested separately (see
[MCP server: Support by client](mcp.md#support-by-client)).

### Unsupported cells

An UNSUPPORTED cell has one of these reasons:

- **Availability.** The SAP 7.40 system holds only `$TMP` objects, so `--tr` and
  transportable-package cells are UNSUPPORTED there.
- **Not in this release.** The transaction code (`TRAN`) create, read and delete cells are
  UNSUPPORTED on every platform and system in 0.15.0; TRAN is planned for 0.16.0 (see
  [TRAN is not supported](behavior-differences.md#tran-is-not-supported)).
- **Stateful session.** The SAP 7.40 system has no stateful ADT HTTP session, so ADT HTTP write cells
  run there only in their pinned and fallback form. The RFC paths cover the same scenarios.
- **Platform.** SAP GUI for Java for Linux is x86_64 only, so Linux arm64 has no SAP GUI library.
  `LD_LIBRARY_PATH` is a Linux loader variable and does not apply on macOS or Windows.
- **Console.** The prompt (`ask`) and Ctrl-C need a terminal. This applies to the interactive group
  of the 0.13.0 matrix, which the 0.15.0 matrix did not rerun. There, the emulated Windows host's
  remote execution gave the process none, so that group ran over ssh with a pseudo-terminal. The
  native Windows host ran it over a pseudo-terminal as well.
- **Fault injection.** Fault injection drops RFC replies only. ADT HTTP has no fault cell.
- **Python parity.** The parity cells run the same operation through the Python tooling. Its deploy
  function module is not installed on the 7.40 system, so parity runs on the 7.50 system only, except
  `read-table`, which passes on both systems.
- **Testability.** A condition that a live run cannot produce is covered only by an offline fake.
  Those cells carry no live evidence (see [Fixture-only cases](#fixture-only-cases)).

### Fault injection

The fault-injection build (`--features fault-injection`) reads `ABAPREACH_FAULT`, a
comma-separated list. Its targets are `drop-reply:PUT`, `drop-reply:ACTIVATE`,
`drop-reply:CREATE`, `drop-reply:DELETE` and `fail-read:RECONCILE`. Each drops one RFC
reply or fails one reconciliation read. The matrix runs these cells on the RFC paths of the SAP 7.50 system; the ADT HTTP cells are UNSUPPORTED. Results on the RFC paths of the matrix:

| Target | Result | Exit |
| :--- | :--- | :---: |
| `drop-reply:PUT` | `INACTIVE`: the PUT reached SAP but the activation did not | 1 |
| `drop-reply:ACTIVATE` | `ACTIVE (reconciled)`: the active source equals the file | 1 |
| `drop-reply:CREATE` | `CREATE: PARTIAL`: the created object is present | 1 |
| `drop-reply:CREATE,fail-read:RECONCILE` | `CREATE: PARTIAL`: the reconciliation read failed, so the state is unknown | 1 |
| `drop-reply:DELETE` | `DELETED (reconciled)`: a new session no longer finds the object; the batch stops | 1 |
| `drop-reply:DELETE,fail-read:RECONCILE` | `PARTIAL`: the reconciliation read failed, so the state is unknown; the batch stops | 1 |

`fail-read:RECONCILE` is matrix-tested only combined with `drop-reply:CREATE` or `drop-reply:DELETE`. Each
reconciliation result exits 1, and none is retried. ADT HTTP cannot be fault-injected, so
these targets are UNSUPPORTED on ADT HTTP. The `CREATE: REJECTED` branch for an object that is
absent after an uncertain create, and the uncertain-POST branches of `--create-include`, have no
live evidence (see below).

The same `ABAPREACH_FAULT` list also takes `hold-lock:<ms>`, with `<ms>` from 1 to 60000. It
pauses the deploy after LOCK until the time is up or Ctrl-C arrives. The interrupt-while-locked
cells use it to test an interrupt while the lock is held, deterministically: the harness waits for
the stderr marker `FAULT: lock held, pausing N ms` and then sends Ctrl-C. `hold-lock` is not a
reply drop, so it does not change the results in the table above.

### Carve-outs

The carve-outs below are the items the release had to cover. Each shows the matrix result and the
limitation that stays documented where the result is not PASS.

| Carve-out | Result | Documented limitation |
| :--- | :--- | :--- |
| Namespaced source-URI path encoding on a registered namespace | UNSUPPORTED | No test object in a registered namespace exists. Objects in registered namespaces are not covered. |
| FUNC rendering beyond the known forms | PASS for the known forms (`func-deploy`, `bom-crlf-deploy`) | Only those forms are covered. Other server re-rendering forms are not. |
| Empty FUNC source on the 7.50 system | PASS (`read-empty-canonical`) | An empty local file is refused before any write (`STOP: source file is empty`). No empty PUT is sent. |
| CLAS/INTF version-entry semantics on the 7.50 system | Documentation limitation | Exercised only through the deploy flows (`tr-clas`, `clas-deploy`, `foreign-inactive*`). Not tested as a separate case. |
| Transportable create, PUT, include POST and delete over ADT HTTP | PASS for create and PUT on the 7.50 system (`create-transportable`, `tr-func`, `tr-clas`, `tr-without-tr`) | A transportable include POST over ADT HTTP was not tested. Transportable delete passes on every path on the 7.50 system (`delete-transportable`). |
| Transportable CLAS activation resend over ADT HTTP and NW RFC SDK | PASS (`clas-activation-worklist`) on ADT HTTP, NW RFC SDK and SAP GUI library | The Linux arm64 SAP GUI cell is UNSUPPORTED (no library). The 7.40 system has no transportable objects, so this scenario does not run there. |
| v5 repeatability | PASS on the SAP GUI library path (`clas-activation-v5-rerun`) | Linux arm64 is UNSUPPORTED (no SAP GUI library). Not tested over ADT HTTP or NW RFC SDK. |
| Out-of-class worklist entries | UNSUPPORTED (fixture-only) | Not tested live. Offline fake only. |
| CLAS with a non-trivial method implementation | PASS (`clas-deploy`) | Covers the shape used by `clas-deploy`. |
| Transportable function group activation | PASS on the 7.50 system (transport setup creates and activates a function group on the test request) | Not run on the 7.40 system (no transportable objects there). |
| NW RFC SDK coverage | PASS: the NW RFC SDK path ran every scenario group on every platform | Its UNSUPPORTED cells are the cases listed under unsupported cells and fixture-only cases, plus the 7.40 system cases that need a transportable object or a second developer user. |
| Create disposition over ADT HTTP on the 7.40 system | Create PASSES over ADT HTTP (`create-fugr`, `create-func`, `create-clas`, `create-intf`). Uncertain-create cells are UNSUPPORTED on ADT HTTP and PASS on both RFC paths | Uncertain-create reconciliation over ADT HTTP is not tested: no ADT HTTP fault injection, and no stateful session on that system. |
| Class Builder DB-level checks | PASS (`clas-verify4`, `4 CHECKS: PASS`) on the 7.50 system; UNSUPPORTED on the Linux arm64 SAP GUI cell | The check is a read-only verification run by the test harness on CLAS objects. ABAPReach does not run it during deploy and never returns exit 3 (`CLASS_METADATA_INCOMPLETE` is not implemented). |
| Ctrl-C while a FUNC lock is held | PASS on every host and path that exists (`v0.13.0-rc.4`, fault-injection build); Linux arm64 SAP GUI cell UNSUPPORTED (no library) | Interrupt-while-locked was proven with the fault-injection build (`hold-lock`), not the release build. |
| Ctrl-C while a CLAS include lock is held | PASS on every host and path that exists (`v0.13.0-rc.4`, fault-injection build); Linux arm64 SAP GUI cell UNSUPPORTED (no library) | Same as above. |

Not observed in the matrix: the transport object list (E071), the TADIR duplicate count and the
number of write POSTs have no read-only endpoint. The matrix records them as unobserved, not as
proved.

### Fixture-only cases

These cases are UNSUPPORTED because a live run cannot produce their condition. The offline fake
models them, so they carry no live evidence:

- `func-where-used-unavailable`: a failing where-used request.
- `clas-activation-messages-w` and `clas-activation-messages-e`: a warning-only and an error
  activation response.
- `clas-activation-outside-class`: a worklist entry outside the class URI.
- `include-uncertain-absent`, `include-uncertain-same`, `include-uncertain-differs` and
  `include-uncertain-unreadable`: the uncertain `--create-include` POST, in each reconciliation state.
- `create-uncertain-absent`: the object-absent branch after an uncertain create.
- `tr-owner-mismatch`: a lock owned by another request owner.
- The first-activation warning of a new include (`W: no syntax check before the first activation of a
  new include`): the state (empty main program list with `--main`) did not arise on the matrix
  systems, because activating a parent that holds the `INCLUDE` statement fills the list.
- A transportable include delete over RFC SADT: covered by fixtures; the 0.15.0 matrix has no live run.

The `--create-include` uncertain-POST tokens (`PARTIAL`, `REJECTED`, `INCLUDE_SOURCE_DIFFERS`)
therefore have no live evidence. The implementation does not send the include POST again in that
state.

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

- Windows and Linux: not part of the 0.11.1 record; the matrix in [Tested environment](#tested-environment) covers
  them. SAP GUI for Java ships per platform, so the Linux package and the RFC
  library inside it are checked separately; the macOS result does not carry over.
- The `ask` prompt and a transportable deploy over ADT HTTP on 7.50: the ADT
  HTTP endpoint of that system was unreachable during these runs. The matrix in
  [Tested environment](#tested-environment) covers both.
- `libsapnwrfccm.dylib`, a second RFC library that SAP GUI 8.10 revision 15
  ships next to `libsapnwrfc.dylib`; ABAPReach loads `libsapnwrfc.dylib`.

macOS on Intel is not supported: SAP GUI for Java 8.10 is the last release for
Intel Macs.

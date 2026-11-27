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
a `.zip` for Windows and a `.tar.gz` for macOS and Linux. The archive holds three
entries: the executable (`abapreach`, or `abapreach.exe` on Windows), `LICENSE` (MIT)
and `THIRD-PARTY-NOTICES` (the licenses of the bundled open source components).
`<target>` is one of
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

Since 0.18.0, `doctor` also prints one `instance ...` line: the application server instance that
answered, read on the session that passed (ADT HTTP, else RFC). On a release without
`/system/information` (the 7.40 system over ADT HTTP) it prints
`instance unknown (no /system/information on this release)`. The line never changes the verdict.
`doctor` masks the host part of the name: `instance ***_<SID>_<NN>`, or `instance ***` for a name that
does not have the form `<host>_<SID>_<NN>`.
See [Ping the system](#ping-the-system).

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
response, and for an object that no open request holds, `--tr` names the modifiable request of
yours that takes it. Deploy can leave an inactive version after failed activation. Read
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
the body. A file in the reference format, whose first line is the `*"` interface header, is
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

Since 0.18.0 `read-table` also asks the ABAP dictionary about the table before it reads it, with
the fail-closed rules of [`sql`](#run-sql-with-joins): a table or a DDIC database view whose base
tables are all allowed is read, and a view over a denied table, a generated CDS SQL view, a CDS
entity, a structure or an unknown name is refused. A refusal is now `STOP` with exit 2 (it was
`REJECTED` with exit 1), for example
`STOP: view V reads table USR02 (read_table_denied); nothing was read`. A lookup that fails stays
`REJECTED` with exit 1.

`delete` removes objects only from a plan written by `--simulate`:

```text
abapreach delete --simulate PROG:ZEXAMPLE --plan delete.plan --conn A4H_001
abapreach delete PROG:ZEXAMPLE --plan delete.plan --conn A4H_001
```

Add `--tr TRKORR` when the object is transportable. Delete has a reconciliation and recovery
path; see [Deploy safety: Delete](deploy-safety.md#delete).

To use ABAPReach from an AI assistant, see [MCP server](mcp.md).

## Run SQL with joins

`sql` runs one read-only `SELECT` through the ADT freestyle data preview. It is the multi-table
counterpart of `read-table`:

```text
abapreach sql "SELECT a~obj_name, b~dlvunit AS unit FROM tadir AS a INNER JOIN tdevc AS b ON a~devclass = b~devclass WHERE a~obj_name LIKE 'ZEXAMPLE%'" --rows 20 --conn A4H_001
abapreach sql --file query.sql --conn A4H_001
```

Give the statement as one argument, or in a file with `--file` (at most 4000 characters; the
command takes exactly one of the two). `--rows` takes 1 to 5000 (default 100). The columns of a
joined table are written `alias~field`.

SAP runs a freestyle body as written, so ABAPReach never sends your text. It parses the statement
with its own grammar and sends only the single-line canonical rendering of the parsed tree (upper
case names, one space between tokens, string literals unchanged). A body that is not canonical is
refused before it leaves the process.

Accepted:

```text
SELECT [DISTINCT] item {, item} | *
FROM entity [AS alias]
{[INNER | LEFT [OUTER]] JOIN entity [AS alias] ON col = col {AND col = col}}
[WHERE condition] [GROUP BY col {, col}] [HAVING condition]
[ORDER BY col [ASCENDING | DESCENDING] {, ...}]
```

An item is a column (with an optional `AS` alias) or an aggregate: `COUNT`, `SUM`, `MIN`, `MAX` or
`AVG` over one column, or `COUNT( * )`. A condition follows the `read-table` rules. A statement has
at most 10 tables and views in `FROM` and `JOIN` together.

Refused with exit 2 and nothing sent: subqueries, `UNION`, `EXISTS`, `WITH`, `INTO`, `UP TO` (use
`--rows`), host variables (`@`), path expressions, CDS parameters, comments, backquote literals,
parenthesized joins, `RIGHT` and `CROSS` joins, `USING CLIENT` and `CLIENT SPECIFIED`. The message
names the first refused construct, for example `STOP: sql: INTO is not supported (the rows are
returned as a table)`.

**Which entities may be read.** The `read_table_denied` list applies to every table and view in
`FROM` and `JOIN`. Before any statement is sent, ABAPReach asks the ABAP dictionary (`DD02L`,
`DDLDEPENDENCY` and `DD26S`, through data previews) about each entity and fails closed:

- A transparent, pool or cluster table is allowed.
- A DDIC database view is allowed only when its base tables are listed in `DD26S` and each one is
  allowed in turn. A view over a denied table is refused, for example
  `STOP: view V reads table USR02 (read_table_denied); no statement was sent`.
- Everything else is refused: CDS entities (a DDL name is not in `DD02L`), generated CDS SQL
  views, views without `DD26S` rows, structures and unknown names.
- The check follows views down to a view of a view. A deeper nesting, more than 500 base tables of
  one view, or more than 24 lookups in one statement is refused, so a view with more than about 21
  base tables cannot be read.
- A refusal exits with 2. A dictionary lookup that fails (the system did not answer) exits with 1,
  `REJECTED: cannot read ...`, and sends nothing.

The output is the same as `read-table`: one header line with the column names, then one
tab-separated line per row, at most 1 MiB (`W: output cut at 1048576 bytes; N rows left out` on
stderr). The 7.50 data preview answers one row more than asked; ABAPReach drops it. The 7.40 data
preview stops at 5000 rows. A configured secret in the output is masked.

The 7.50 and 758 systems list the freestyle data preview in their ADT discovery. The 7.40 system
has an empty HTTP discovery but still answers `sql` over ADT HTTP (checked live on the 7.40 test
system).

## Search objects

`search` finds objects by name through the ADT object search (`quickSearch`):

```text
abapreach search 'ZEXAMPLE*' --type CLAS --max 20 --conn A4H_001
```

`*` is the only wildcard. A pattern without `*` names exactly one object. A pattern has at most 40
characters: letters, digits and `_`, plus `*` and an optional namespace at the start (`/NS/NAME`).
It is converted to upper case. `--type` limits the search to one type: `PROG`, `FUNC` (function
module), `FUGR`, `CLAS`, `INTF`, `DTEL`, `DOMA`, `TABL` (transparent tables), `TTYP`, `TRAN`, `DDLS` or
`MSAG`. `--max` takes 1 to 1000 (default 50).

One line per object, then a count:

```text
CLAS/OC ZCL_EXAMPLE package=ZEXAMPLE Example class
SEARCH: 1 object
```

The first field is the ADT object type as SAP names it (`PROG/P`, `FUGR/FF` for a function module,
`CLAS/OC`, `TABL/DS` for a structure). A missing package prints as `package=?`. Without `--type`,
the search also returns structures and other types; `--type TABL` returns transparent tables only.
Exit 0 also when nothing matches (`SEARCH: 0 objects`).

A release that ignores the type filter answers objects of other types. ABAPReach drops them, and
every object whose name does not match the pattern, and says so on stderr:
`W: the reply held objects of other types; N left out (the result may hold fewer than --max objects)`.
A system without the search route answers `the object search answered HTTP 404: this system does
not offer the object search on this route`. The 7.50 and 758 systems list the search in their ADT
discovery. The 7.40 system has an empty HTTP discovery but still answers the search over ADT HTTP
(checked live on the 7.40 test system).

## Find where an object is used

`where-used` lists the objects that use one object:

```text
abapreach where-used FUNC ZEXAMPLE_FM --max 100 --conn A4H_001
```

`TYPE` is `PROG`, `FUNC`, `FUGR`, `CLAS`, `INTF`, `DTEL`, `DOMA`, `TABL` or `TTYP`; `--max` takes 1
to 5000 (default 200). `MSAG` stops with `where-used of MSAG is not available in this version`, and
`TRAN` stops with `where-used of TRAN is not supported: no ADT where-used path exists for a
transaction`. A name of a type that goes through ADT has letters, digits and `_` only.

One line per using object, sorted, each once, then a count:

```text
USED-BY PROG ZEXAMPLE_REPORT package=ZEXAMPLE via=ADT
WHERE-USED: 1
```

Exit 0 also with no hit. `--max` cuts the sorted list and prints
`W: N objects found; the first M are shown (--max)` on stderr. Only the objects are listed, not the
lines of code that use the object.

**ADT first, cross-reference tables as the fallback.** `PROG`, `FUNC`, `FUGR`, `CLAS` and `INTF`
always go through the ADT `usageReferences` search, and `via=ADT` marks each line. A DDIC type goes
through it only when the discovery document of the release lists the collection of the type. On the
7.40 system no DDIC type is listed, on the 7.50 system data elements and structures are, and the
758 system lists all of them. Otherwise `where-used` reads the standard cross-reference tables
through the data preview, as `read-table` does: `WBCROSSGT` (type use), `DD03L` for the tables that
use a data element and `DD04L` for the data elements that use a domain. The line says which table
it came from (`via=WBCROSSGT`, `via=DD03L`, `via=DD04L`), and stderr gets one of:

- `W: no ADT where-used for <TYPE> on this release; using cross-reference tables`
- `W: the ADT where-used for <TYPE> answered HTTP <n>; using cross-reference tables`
- `W: no ADT where-used path for a namespaced <TYPE> name; using cross-reference tables`

**`TABL`: tables and structures.** ADT has one collection for transparent tables and another for
structures. ABAPReach reads the table class from `DD02L` and picks the collection. A transparent,
pool or cluster table uses `tables`, a structure uses `structures`, and any other class uses the
cross-reference tables.

The fallback maps an include to its main object (a class pool include to `CLAS`, a function group
include to `FUGR`, otherwise the master `PROG`) and reads the packages from `TADIR` in a few
bounded reads. Where it reaches a limit it says so with a `W:` line, for example the packages of
some objects were not looked up (`package=?`).

**The 64 MiB reply cap.** `usageReferences` has no result limit: a widely used structure such as
`BAPIRET2` answers 15 to 29 MB. ABAPReach reads the reply and refuses one over 64 MiB with exit 1,
`REJECTED: the where-used reply is <n> bytes, over the limit of 67108864 bytes; --max cannot help,
because the system sends the whole list before it is cut`. `--max` only shortens the printed list.

`delete --simulate` keeps its own where-used check and is not changed.

## Short dumps

`dumps` lists the ST22 short dumps from table `SNAP`, newest first, and `dump` shows one of them:

```text
abapreach dumps --date 20260101 --rows 10 --conn A4H_001
abapreach dump 20260105/101500/apphost_A4H_00/DEVUSER/001 --conn A4H_001
```

`dumps` shows the dumps of the logon user from the last 7 days (one more day covers the time
zone). `--date YYYYMMDD` starts on that day. `--user U` shows another user's dumps, `--all` shows
every user's, as ST22 does, and the two cannot be combined. `--rows` takes 1 to 1000 (default 20).
One line per dump, or `(no dumps)` when there is none:

```text
20260105 101500 DEVUSER 001 error=COMPUTE_INT_ZERODIVIDE program=ZEXAMPLE include=ZEXAMPLE line=12 id=20260105/101500/apphost_A4H_00/DEVUSER/001
```

`id` is the argument of `dump`: `DATUM/UZEIT/AHOST/UNAME/MODNO`. `dump` prints the error, program,
include, line, user and client, the short text from `SNAPT` (in English, else in the logon
language, else in the first language the table has) and the source lines from five before to five
after the failing line, with `>` on it. It prints only these decoded fields, never the raw dump
content. A class method include (a name with `=`) cannot be read, and the excerpt says so
(`(source <name> not read: not a program or include name)`). A dump of another user needs `--all`
(`STOP: the dump belongs to user X, not to the logon user; a dump of another user needs --all`).
A dump that does not exist is `REJECTED: dump not found: <id>` (exit 1).

**`--all` is a display filter, not an access boundary.** `SNAP` is not on the default
`read_table_denied` list, so `read-table SNAP` and `sql` can already read every user's dumps where
the data preview allows it. Which dumps a user can read is decided by the SAP authorizations of the
logon user. A dump can hold the values of variables; the output is masked for configured secrets,
but nothing else is removed.

**Release differences.**

- **7.50.** The data preview refuses `SNAP` (`You cannot display SNAP with the standard tools`). Over
  ADT HTTP, with an RFC backend configured and no `--backend` given, the command moves on to the RFC
  paths (and so do the MCP tools `abap_dumps` and `abap_dump`). With `--backend adt`, or when no RFC
  backend is configured, it stops with exit 2: `SAP does not show SNAP in the data preview of this
  release; use --backend rfc to read it through RFC_READ_TABLE`. The RFC paths read `SNAP` through the
  standard function module `RFC_READ_TABLE`, a field group at a time.
  ABAPReach recognises the refusal by its English text, so the fallback needs an English logon
  (the `lang` of the server entry, default `EN`). With another logon language the data preview
  error is shown as it is.
- **7.40 and 758.** The data preview reads `SNAP` directly.
- **Dump content.** A dump is stored in the columns `FLIST` to `FLIST08` (eight columns of 200
  characters). ABAPReach reads all of them; the reference tooling reads `FLIST` to `FLIST05` only.
  On the RFC fallback, a column group the release does not have is skipped.
- **Limits.** `dumps` reads at most 5000 matching rows and refuses a larger match
  (`5000 or more dumps match; narrow the list with --date or --user`). `dump` refuses a dump of
  1000 rows or more as too large.

## Ping the system

`ping` shows the system, the application server instance that answered and the round trip time:

```text
abapreach ping --conn A4H_001
abapreach ping --backend rfc --conn A4H_001
```

```text
backend ADT HTTP
instance apphost_A4H_00
system id A4H
kernel 753
database DB6
os Linux
round trip 142 ms (includes logon)
```

Without `--backend`, ABAPReach tries ADT HTTP first and moves to RFC only when nothing was sent,
as for every command; the first line names the backend that answered. `--backend adt` and
`--backend rfc` pin the path (`rfc` is the SAP GUI library or the NW RFC SDK), so you can see the
instance each path reaches. A line with no value on a path is left out.

| | ADT HTTP | RFC |
| :--- | :--- | :--- |
| Source | `GET /sap/bc/adt/system/information` | `RFC_SYSTEM_INFO` |
| Instance | `ApplicationServerName` | `RFCDEST` |
| Lines | system id, kernel, database, os | system id, release, kernel, database |
| Round trip | the call, with the logon | the call alone, after the logon |

The round trip is the time of that one call. Over ADT HTTP it is the first request of the
session, so it includes the TLS connect and the logon, and the line says
`(includes logon)`. Over RFC it is the time of the function call alone. Exit 0 when the system
answered; otherwise the usual connection failure.

The 7.40 system has no `/system/information` (HTTP 404). `ping` over ADT HTTP then prints
`instance unknown (no /system/information on this release)` and still exits 0; use
`--backend rfc` to see the instance. HTTP 406 gives `instance unknown (HTTP 406)`. The host address
and the database server of the reply are never printed.

`ping` prints the instance name in clear, exactly as SAP reports it (`<host>_<SID>_<NN>`, with the
host). `doctor` prints the same reading from the one session that answered, with the host masked:
`instance ***_<SID>_<NN>`, or `instance ***` for a name that does not have that form. A failed
reading shows `instance unknown (<reason>)` in both and does not change the verdict of `doctor`.
The reason is one of a fixed set: `no /system/information on this release`, `HTTP 406`, `HTTP <status>`, `request failed`, `no answer`, `not authorized` or `unreadable reply`; it is never the text
of an error. Over MCP, `abap_ping` prints the name in clear like `ping`; see
[MCP](mcp.md#new-read-tools-0180).

## Inactive objects, activation and ABAP Unit

`inactive` lists the inactive objects of the logon user, one line `<ADT type> <name> <user>` each
(`PROG/P`, `PROG/I`, `CLAS/OC`, `INTF/OI`, `FUGR/F`, `FUGR/FF`, DDIC types, and so on). An include also
names its main program. Methods and other parts of a class are not listed, only the class. A reply that
names no user (the flat 7.40 feed) prints `?` as the user; the person responsible for the object that
feed may carry is never read as the owner of the inactive version. An entry that marks an inactive
deletion ends with ` deleted`.

```text
abapreach inactive --conn A4H_001
abapreach inactive --all --conn A4H_001
```

`--all` asks the system for every user's entries (`?USERNAME=*`). A release that does not offer the
filter answers with an error and the command says so.

`activate` activates inactive objects by the tokens `inactive` prints. It runs one `POST
/sap/bc/adt/activation` per object, then reads the inactive list again: an object counts as active only
when it is no longer listed. The exit code is 0 only when every object is active.

```text
abapreach activate --simulate PROG/P:ZEXAMPLE CLAS/OC:ZCL_EXAMPLE --conn A4H_001
abapreach activate PROG/P:ZEXAMPLE CLAS/OC:ZCL_EXAMPLE --conn A4H_001
```

- Accepted types: `PROG/P`, `PROG/I` (activated under its main program), `CLAS/OC`, `INTF/OI`, `FUGR/FF`.
  `FUGR/F` and every other type stop with the reason; DDIC objects go through the DDIC deploy.
- Only Z and Y objects of the logon user. The user comes from the inactive list; when the list names none
  (7.40), the author of the inactive version in the version feed decides. An object that is not listed,
  belongs to someone else or has an unknown owner rejects the whole call before any activation. An
  entry marked `deleted` is refused with the reason: activating it would complete the deletion.
- ABAPReach reads `T000` first and refuses a productive client (`CCCATEGORY P`). A client that `T000`
  does not list or that cannot be read is refused too.
- `--simulate` runs every check above and prints `SIMULATED_OK <token> user <user>` per object.
- A transportable class answers the first request with a worklist and activates nothing; ABAPReach sends
  the second request with exactly that list, as the deploy does, and only when the list stays inside the
  class.

`aunit` runs ABAP Unit with harmless tests of every duration (no coverage):

```text
abapreach aunit CLAS ZCL_EXAMPLE --conn A4H_001
abapreach aunit --package ZEXAMPLE --conn A4H_001
abapreach aunit --tr A4HK900001 --conn A4H_001
```

Each alert prints `<kind> <level> <prog> <class>-><method>: <text>`, then `TESTS: <n> methods, <m> alerts`
and `AUNIT: ACTIVE` (no alert) or `AUNIT: REJECTED`. The exit code is 0 only without alerts. A
`--package` or `--tr` batch prints a block `== <TYPE> <NAME> ==` per object, `SUMMARY: <n> OK, <m> FAILED`
and a `FAILED <TYPE> <NAME>: <reason>` line each; an error on one object counts as failed and the batch
goes on. A batch without objects exits 1: an empty gate is not a pass.

- Test code runs on the SAP system, so `aunit` counts as running code: the policy `run` (`off`, `plan`,
  default `plan`, or `direct`) must not be `off`, and a productive client is refused. `aunit` needs no
  plan file under `plan` or `direct`: it runs the test classes of the named objects only.
- Types `PROG`, `CLAS` and `FUGR`; plain Z and Y names. An object that does not exist is rejected.
- `--package` takes the programs, classes and function groups of the package (`TADIR`, not deleted);
  program includes are skipped. `--tr` takes the request and its tasks: `R3TR` programs, classes and
  function groups, and the parts of a class (`LIMU METH`, `CPUB`, `CPRO`, `CPRI`, `CLSD`, `CINC`,
  `CDEF`, `CIMP`), which stand for the class. Other `LIMU` entries (`REPS`, `FUNC`) are not mapped and
  are counted in a `W:` line. A batch holds at most 100 objects on the CLI. The MCP tool `abap_aunit`
  refuses a batch of more than 60 objects: its confirmation form lists at most 60, and a run is
  never confirmed from a cut list.

`inactive`, `activate` and `aunit` take `--backend adt|rfc` like `deploy`. Without it they start on ADT
HTTP, and when the first reply shows a session that keeps no state (a 7.40 system), they stop before
anything is written and run again over RFC, with the `NOTE: ADT HTTP session is stateless on this
release` and `NOTE: ADT HTTP failed before anything was written; falling back to RFC SADT` lines. A
command that ran over RFC prints `backend: rfc` and `reason: <why>` ahead of its output. A stateless
session may answer 404 for an object that exists, so with `--backend adt` (or without an RFC route) a
404 stops with the reason instead of `does not exist`. `run PROG` and `run SNIPPET` choose the same
way; `run CLAS` needs no stateful session and stays on ADT HTTP. The plan file of a run records the
backend its simulate used, and the run keeps to it.

## DDIC objects and transaction codes

Data elements (`DTEL`), domains (`DOMA`), tables (`TABL`: transparent tables and structures), table
types (`TTYP`) and transaction codes (`TRAN`) use the same commands. The source of a DDIC object is its
XML, which `read` writes; `TRAN` has no source and is created with `--prog`:

```sh
abapreach read DTEL ZDE_EXAMPLE ./ZDE_EXAMPLE.xml --conn A4H_001
abapreach deploy DTEL ZDE_EXAMPLE ./ZDE_EXAMPLE.xml --simulate --base HASH_FROM_READ --conn A4H_001
abapreach deploy DTEL ZDE_EXAMPLE ./ZDE_EXAMPLE.xml --base HASH_FROM_READ --conn A4H_001
abapreach create TRAN ZEXAMPLE --prog ZEXAMPLE_REPORT --text 'Example transaction' --simulate --conn A4H_001
abapreach delete --simulate TRAN:ZEXAMPLE --plan delete.plan --conn A4H_001
```

Every operation on these types, `read` included, creates and removes a temporary class in `$TMP`, so
the system must allow repository changes and the user needs developer authorization for that class.
`TRAN` has no deploy; use `create` and `delete`. The `DDIC` conversion rules, the footprint and the
authorizations are in [Deploy safety: DDIC and TRAN](deploy-safety.md#ddic-and-tran). The path each
type takes on the 7.50 and 7.40 systems is in
[Behavior differences: Access path comparison](behavior-differences.md#access-path-comparison-0160-rc6).

## Run a class, a report or a snippet

`run` executes ABAP code on the SAP system with the authorizations of the logon user and prints its
output. It is **not a dry run**: whatever the code writes to the database or elsewhere stays
written, exactly as when you start it in SA38 or Eclipse. `--simulate` reads and checks and never
executes anything.

| Command | What runs | Where it works |
| :--- | :--- | :--- |
| `run CLAS <class>` | an existing class that implements `IF_OO_ADT_CLASSRUN` | any system where the class exists |
| `run PROG <report>` | an active executable report (type 1), with parameters and a variant | development systems only |
| `run SNIPPET <file>` | the ABAP statements of the file | development systems only |

`run PROG` and `run SNIPPET` create a temporary runner class in `$TMP`, run it once and delete it
(the same cycle as the DDIC types), so the system and client must allow repository changes and the
user needs developer authorization for that class. On a quality or production system they stop
with the reason before any request that changes something. A productive client (`T000-CCCATEGORY =
P`) refuses every `run` mode.

### Run a class

```sh
abapreach run CLAS ZCL_EXAMPLE --simulate --plan run.plan --conn A4H_001
abapreach run CLAS ZCL_EXAMPLE --plan run.plan --conn A4H_001
```

The simulate checks that the class exists, is active and implements `IF_OO_ADT_CLASSRUN` (the class
itself or a superclass), reads its package, author and active source hash, and checks that the
client is not productive (`T000-CCCATEGORY` is not `P`; a client whose role cannot be read is
refused too). The plan file holds the connection, client, backend, class name, source hash and
package. The run executes only when its arguments match the plan file (a mismatch is a `STOP`,
exit 2, before any connection) and the live facts still match (otherwise `REJECTED: plan changed
since simulate (source hash); simulate again`, exit 1). A plan pins the backend it was written on.
The class reply is printed as the server returns it.

The plan binds the **active main source** of the class (`source/main`) and its package, not the local
class includes (`CCDEF`, `CCIMP`, `CCMAC`) that the main source can call. A local include that changes
between the simulate and the run does not change the plan, so the run still goes ahead. Simulate
again after any change to the class, and read the includes before you trust an old plan file. (`run
PROG` has the same limit: its plan covers the main program, not its includes.)

### Run a report

```sh
abapreach run PROG ZREPORT --param P_DATE=20261101 --param S_BUKRS=1000 --variant V_DAILY \
  --simulate --plan run.plan --conn A4H_001
abapreach run PROG ZREPORT --param P_DATE=20261101 --param S_BUKRS=1000 --variant V_DAILY \
  --plan run.plan --conn A4H_001
```

The report runs inside the runner class with `SUBMIT ... EXPORTING LIST TO MEMORY AND RETURN`; its
list is read from memory and printed as text lines. The class contains two fixed `SUBMIT` shapes
(with and without a variant) and no value of yours: the report name, the parameters and the
variant travel in the encoded payload.

- `--param NAME=VALUE` (repeatable) sets a parameter or the single value of a select-option, by its
  selection-screen name (at most 8 characters of `A-Z 0-9 _`). A value has at most 45 characters
  (the length of `RSPARAMS-LOW`) and no control character, and is in the internal format of the
  field (`20261101` for a date). A select-option takes one `I EQ` row. A name that is not on the
  selection screen is `REJECTED: parameter <NAME> is not on the selection screen of report
  <REPORT>` and the report does not run. A parameter overrides the same field of a variant.
- `--variant <NAME>` uses an existing selection variant of the report (`REJECTED` when it does not
  exist). ABAPReach cannot create a variant; create it in SE38 or SA38.
- The simulate reads the report: it exists, is active, has `TRDIR-SUBC = 1` (other types such as
  module pools are refused), its package, author and active source hash. The plan binds the report
  name, its source hash, `SUBC`, package, every `--param`, `--variant`, `--alv` and `--alv-columns`.
- Right before `SUBMIT` the runner reads the active source of the report again and compares its
  hash with the one of the simulate. A difference is `REJECTED: report changed since the simulate`
  and the report does not run. The hash covers the main program, not its includes.
- The runner checks `AUTHORITY-CHECK OBJECT 'S_PROGRAM'` for the report's authorization group with
  action `SUBMIT`, as SA38 does.
- Only `Z` and `Y` report names run; `--allow-standard` allows a standard report. A namespaced name
  is accepted as a report name.

**ALV output.** Most reports show an ALV grid, which a background `SUBMIT` cannot display. By
default the runner captures the grid (`CL_SALV_BS_RUNTIME_INFO`: data and field catalog) instead:
the output is one header line with the column texts and one line per row, tab-separated, then the
list text if the report also wrote one.

- Only the columns the field catalog shows are printed (not the technical ones and not the ones
  marked `NO_OUT`), in field-catalog order. `--alv-columns all` (CLI only, never MCP) prints every
  column of the table.
- An amount is written with the currency of its reference field, a quantity with its unit, so the
  decimals are the ones SA38 shows (no decimals for `JPY`). The notation of the number (decimal
  separator) follows the user settings of the logon user, and thousands separators are left out. A
  value without a reference is printed in its internal form, with a point as decimal separator.
  A tab or a line break in a value becomes a blank.
- `--alv off` keeps the plain list behaviour. A report that shows no grid is a normal run with the
  list only.
- Limits: one grid per run is captured (when a report shows two, only one of them; the live check
  decides which); hierarchical, tree and docking-container ALVs are not captured; a report that
  tests `SY-BATCH` or needs an interaction with the grid may behave differently.

**Other limits.** A report that sends a screen or a popup, or calls the front end, ends in an ABAP
runtime error (for example `DYNPRO_SEND_IN_BACKGROUND`): that cannot be detected before the run, and
it is `RUN_FAILED` below. `WRITE` output of a report is its list; classic output of the runner itself
is not used.

### Run a snippet

```sh
abapreach run SNIPPET snippet.abap --simulate --plan run.plan --conn A4H_001
abapreach run SNIPPET snippet.abap --plan run.plan --conn A4H_001
```

The file holds ABAP statements. They become the body of its own method `run_snippet` in the runner
class, so one snippet compiles on every release:

```abap
DATA(lv_count) = lines( VALUE string_table( ( `a` ) ( `b` ) ) ).
print( |count: { lv_count }| ).
```

- The only output is `print( iv_text )`, which takes a string and prints one line. Do not use `out`:
  its type differs between releases. A classic `WRITE` is not captured.
- Every statement ends with a period; the snippet is the body of one method. Refused before
  anything is sent, statement by statement (colon chains and several statements on one line
  included): `METHOD`, `ENDMETHOD`, `METHODS`, `CLASS`, `ENDCLASS`, `INTERFACE`, `ENDINTERFACE`,
  `FORM`, `ENDFORM`, `DEFINE`, `END-OF-DEFINITION` and `INCLUDE`. A line has at most 255
  characters and no control character (a tab counts). A text literal, a string template or a
  statement left open at the end is refused.
- The simulate prints the whole snippet and its plan binds the snippet text by its hash. A changed
  file is `STOP: plan does not match the arguments (snippet)`.
- A snippet that does not compile is `SNIPPET_ERROR: line <n>: <text>` (exit 2), with `n` the line
  of your file. Nothing ran and the runner class is removed. It is not a bug of ABAPReach.
- The snippet is your own code and runs inside the runner, so it can also call the methods of the
  runner class or write protocol lines of the run. ABAPReach does not defend against that: the code
  is yours, and it is shown in full in the simulate and in the MCP confirmation.

### Rules before anything is sent

- The name rule: only `Z` and `Y` names run; `--allow-standard` allows a standard class or report.
  A namespaced class name is refused: its classrun path is not verified. A snippet has no name.
- A temporary runner class is never run: a name that is the connection's `runner_prefix` or the
  default `YCL_ABAPREACH_RUN_` followed by exactly eight characters of `0-9` and `A-Z` (the shape
  ABAPReach generates) is a `STOP` (`a temporary runner class is not run; delete it (doctor)`). A
  class that only starts with the prefix, such as `YCL_ABAPREACH_RUN_HELPER`, is an ordinary class.
  A leftover of a crashed operation carries its own payload; remove it with the commands `doctor`
  prints. The `runner_prefix` of a server entry starts with `Y` or `Z`, has at most 22 characters
  of `A-Z`, `0-9` and `_`, and defaults to `YCL_ABAPREACH_RUN_`; a runner class is that prefix plus
  the eight generated characters (at most 30 characters).
- The connection policy `run` decides how: `plan` (the default) needs the two steps above;
  `direct` lets the CLI run without `--plan`; `off` refuses `run` (and `aunit`) on the CLI and in
  MCP. See [MCP server: `mcp_run`](mcp.md#mcp_run) for the MCP opt-in.
- `--timeout <seconds>` (default 600, at most 3600) bounds the run request over ADT HTTP. The
  connect timeout stays at 45 s. The RFC library has no call timeout and no cancel, so `--timeout`
  on RFC is `STOP: --timeout is not supported over RFC` and an RFC run waits until SAP answers,
  bounded by the dialog work process limit. The ICM processing timeout or the dialog work process
  limit of the server may end a run first.
- `--max-output <bytes>` (PROG and SNIPPET; default 6 MiB, at most 6 MiB) limits the output. The
  runner counts the bytes it writes (the marker, the nonce, the hex text and the framing of every
  line) and stops before the limit with `W: output cut after <n> lines`; the outcome stays `RAN`.
  The limit keeps the reply under the 8 MiB the parser accepts. MCP uses 1 MiB.
- The payload carries the logon user and an expiry time (UTC): the generation time plus `--timeout`
  plus 20 minutes (over RFC: plus 75 minutes). The runner class refuses to run when `SY-UNAME`
  differs or the time has passed (`REJECTED: run payload expired or foreign user`). A runner class
  that a second Ctrl-C left behind therefore cannot be started again from Eclipse.

### Results

The run request is sent once, from a new session, and never repeated. The runner class is deleted
after every outcome (and its `TADIR` entry proven gone); a class that stays is `RUNNER_LEFTOVER` as
for every runner operation.

| Result | Exit | Meaning |
| :--- | :---: | :--- |
| output printed (class: the reply as is; report and snippet: `RAN`) | 0 | the code ran to its end; ABAPReach does not roll back (as SA38) |
| `RUN_FAILED: exception <class>: <text>` | 1 | a catchable exception reached the runner (snippet); ABAPReach rolled back the work since the last `COMMIT WORK` |
| `RUN_FAILED: runtime error; see ST22 (dumps)` | 1 | the server answered an ABAP runtime error (HTTP 500 with a dump page) or the call failed with a runtime failure (RFC); also a report that sends a screen (dump `DYNPRO_SEND_IN_BACKGROUND`) when the reply carries the English short text `Screen output without connection to user`; rolled back by the server to its last commit |
| `RUN_UNKNOWN: the reply was lost; the code may have run` | 1 | any other HTTP 5xx (an ICM timeout can answer while the work process still runs), a timeout, a lost connection, or a reply without a status line; look at the effects in SAP before running again |
| `SNIPPET_ERROR: line <n>: <text>` | 2 | the snippet does not compile; nothing ran |
| `REJECTED: ...` | 1 | a check or the server refused; the code did not run |
| `STOP: ...` | 2 | input, configuration or policy; or the system has no classrun (HTTP 404) |

No outcome of a report or snippet is reconciled afterwards (there is no object state to compare);
the cleanup and its proof run after every outcome. A `W:` line on the stderr carries the cut note,
messages of the runner and the runner name.

Ctrl-C while the run is in flight: the first one prints `I: run in progress on the server; waiting
for the reply (Ctrl-C again to leave)` for a class, and `I: run in progress on the server; waiting
for the reply to clean up (Ctrl-C again to leave the runner class behind)` for a report or a
snippet, and waits for the reply, which is printed; the exit code is then 130 when the run
succeeded, and the exit of the outcome otherwise (1 for `RUN_FAILED` and `RUN_UNKNOWN`). A second Ctrl-C
leaves at once (exit 130); the code keeps running on the server, and for a report or snippet the
`RUNNER_LEFTOVER` lines on the stderr name the runner class and the commands that delete it (it also
shows in `doctor`).

The 0.17.0 release checks ran `run` live on the 7.50, 7.40 and 758 systems (see
[Tested environment](#tested-environment)); the paths without a live test are listed there. The selection
screen is read through
`RS_REPORTSELECTIONS_INFO` (table `FIELD_INFO`; interface verified on 7.40, 7.50 and 758). A report
without a selection screen gives `REJECTED: parameter <NAME> is not on the selection screen of
report <REPORT>` for every `--param`; a screen that cannot be read gives `REJECTED: the selection
screen of report <REPORT> cannot be read`, and a run without parameters is not affected. The live
cases do not cover which grid is captured when a report shows more than one. An HTTP 500 body marks a
runtime error (`RUN_FAILED`) when it matches the text `runtime error`, `short dump` or `ST22`, or the
short text of a known dump (a report that sends a screen: `Screen output without connection to user`
on the 7.50 system, or on the ADT page of the 758 system
`Sending of dynpro <program> <screen> not possible: No window system type specified`; seen live with
an English logon language); any other 500 counts as `RUN_UNKNOWN`.

## Tested environment

The 0.16.0 release matrix ran on `v0.16.0-rc.6` (the release binaries of 0.16.0), with the release and
the fault-injection builds, on each platform and path, against two systems. The final runs have no FAIL
cell and no leftover test object. The matrix covers the core, platform, fault-injection, delete,
read-table, policy, MCP and reference-comparison groups, and the 0.16.0 scenarios for the DDIC types and
`TRAN` (read, simulate, deploy, create, delete, and the conversion guard). Some runs were repeated because
the test environment failed, not the product: on Windows x64 native, the 7.40 system's `rfc-gui` group
had 4 VM transport errors in its first run and passed on rerun (166 cells), and the 7.50 system's
`rfc-sdk` group was rerun to complete its cleanup cells (185 cells). The rc.6 changes are in the test
harness only; no product change was needed.
The interactive group (prompts and Ctrl-C) is from the 0.13.0 matrix (`v0.13.0-rc.4`) and was not
rerun for 0.16.0. The two systems:

- a SAP 7.50 system with a stateful ADT HTTP session, a second developer user and transport
  requests;
- a SAP 7.40 system with local objects only (`$TMP`), no stateful ADT HTTP session.

The 0.16.1 release checks also used a third system, an S/4HANA system with SAP_BASIS 758 (the 758 system),
on `v0.16.1-rc.2` and `v0.16.1-rc.4` (the release binaries). On 0.16.1 it was covered by the smoke set and
group m7 only. The results are in
[Behavior differences: 0.16.1 release checks](behavior-differences.md#0161-release-checks-0161-rc2-0161-rc4).

The 0.17.0 release checks ran on `v0.17.0-rc.1` to `v0.17.0-rc.4` (the release binaries are the rc.4
binaries), on the same three systems. The full matrix runs once per release on each system; a later
release candidate reruns only the cells its change touches. This is the first full matrix on the 758
system. The final runs have no FAIL cell, and no test object was left on any system. The 0.17 features
(`run`, `activate`, `aunit`, `inactive`) also ran as live cases on all three systems (rc.3 and rc.4 binaries).
Windows x64 and Linux arm64 had no live run in 0.17.0 (as in 0.16.1); CI builds and checks their release
files. The runs are listed per system in
[Behavior differences: 0.17.0 release checks](behavior-differences.md#0170-release-checks-0170-rc1-to-rc4).

The 0.17.1 release checks ran on `v0.17.1-rc.1` (the release binaries). The smoke set ran live on the 7.50
system on all four platforms, which is the first live Windows x64 and Linux arm64 run since 0.16.1. The
results are in [Results of the 0.17.1 release checks](#results-of-the-0171-release-checks).

The 0.18.0 release checks ran on `v0.18.0-rc.2` and `v0.18.0-rc.3` (the release binaries are the rc.3
binaries; a cell that rc.3 does not change keeps its rc.2 result). The full matrix ran on the three systems
and the three paths: the 7.50 and 7.40 systems on macOS arm64, Linux x64, Linux arm64, Windows x64 native and
Windows x64 emulated on arm64, and the 758 system on macOS arm64 and Linux x64. The result is 6237 PASS,
2055 UNSUPPORTED and 5 FAIL in 8297 cells; the FAIL cells are explained in
[Results of the 0.18.0 release checks](#results-of-the-0180-release-checks).

Each cell has one of three results:

- **PASS**: the scenario ran on that platform and path and its checks held.
- **UNSUPPORTED**: the scenario could not run there, for the reason the cell gives. It is not
  a failure. It is also not evidence that the feature works in that case.
- **FAIL**: the scenario ran on a supported path and a check did not hold. No cell failed in the
  0.16.0 matrix on its final run.

### Results by platform and system

Each cell is the PASS and FAIL count of the final rc.6 run for that platform and system. The final
runs have no FAIL cell. UNSUPPORTED counts are not listed for rc.6; the reasons are in
[Unsupported cells](#unsupported-cells).

| Platform | SAP 7.50 system | SAP 7.40 system |
| :--- | :---: | :---: |
| macOS arm64, ADT HTTP / RFC SAP GUI library / RFC SDK | 228 / 243 / 243 PASS | 58 / 218 / 218 PASS |
| Windows x64, native | 520 PASS, plus 185 PASS on the RFC SDK rerun ¹ | 372 PASS, plus 166 PASS on the RFC SAP GUI rerun ¹ |
| Windows x64, emulated on an ARM64 virtual machine | 523 PASS | 376 PASS |
| Linux x64 | 525 PASS | 378 PASS |
| Linux arm64 | 348 PASS | 218 PASS |

¹ On the 7.50 system, a test-environment alarm cut the native run during its cleanup phase, so the RFC SDK
group was rerun to complete the cleanup. On the 7.40 system, the first run had 4 VM transport errors, and
the RFC SAP GUI group was rerun. Every cell in both reruns passed.

Every lane reported no leftover test object. The sweeps at the start and at the end passed on every
final run, each verified in a fresh session. The 7.50 system's fresh listing after the last run showed
only the three test objects the matrix keeps.

**Reference comparison.** The DDIC and `TRAN` operations were compared with the reference behavior on
the 7.50 system: 6 cells PASS, 24 EXPECTED (the intentional differences in
[Behavior differences](behavior-differences.md#ddic-and-tran-differences-0160)), 0 FAIL, and 3 cells
UNSUPPORTED: the preflight refusal (the test user has every authorization), the leftover check (it needs
a build with fault injection), and the `TRAN` parity check (the reference prints parity only for function
modules). On the 7.40 system the reference operations cannot run, because the reference deploy function
module is not installed there.

The MCP cells drive the server with a scripted client that reproduces each client's advertised
capabilities. The real clients were tested separately (see
[MCP server: Support by client](mcp.md#support-by-client)).

### Results of the 0.17.0 release checks

Each row is one full-matrix run (or a rerun of the cells that failed on rc.1). Every final run has 0 FAIL;
the 9 and 2 FAIL cells of the 758 rc.1 runs are explained below.

| System | Run | PASS | UNSUPPORTED |
| :--- | :--- | ---: | ---: |
| SAP 7.50 system | rc.2, macOS arm64, three paths | 711 | 68 |
| SAP 7.40 system | rc.2, macOS arm64, three paths | 491 | 288 |
| SAP 758 system | rc.1, macOS arm64, ADT HTTP | 197 | 61 |
| SAP 758 system | rc.1, macOS arm64, RFC SAP GUI library | 220 | 44 |
| SAP 758 system | rc.2, Linux x64, RFC SDK | 171 | 35 |
| SAP 758 system | rc.2, macOS arm64, m6-runner, ADT HTTP | 11 | 0 |
| SAP 758 system | rc.2, macOS arm64, m6-runner, RFC SAP GUI library | 12 | 0 |
| SAP 758 system | rc.2, macOS arm64, m6-ddic and m6-delete, ADT HTTP | 67 | 5 |

The 758 rc.1 runs had 9 FAIL cells on ADT HTTP and 2 on the RFC SAP GUI library: 7 harness faults and 4
Ctrl-C timing cells. The cells are explained in
[Behavior differences](behavior-differences.md#0170-release-checks-0170-rc1-to-rc4) and the rerun on rc.2
has no FAIL. The live cases
of the 0.17 features: 66 and 70 PASS on the 7.50 system, 53 and 64 PASS on the 7.40 system, 61 and 70
PASS on the 758 system (one Replay FAIL there is an outdated script expectation), on the rc.3 (Replay)
and rc.4 (Extra) binaries.

Not run live in 0.17.0 (offline tests cover them): Ctrl-C twice during `run`, the server-side `BASE`
check, `RUN_UNKNOWN` from an ICM 5xx, the output budget with one-character lines and non-ASCII text, an
expired payload re-posted, a snippet with `ENDMETHOD` or `INCLUDE` in a colon chain, a non-Z/Y name
without `--allow-standard`, a plan mismatch, the MCP policy matrix and the selection-variant cell.
0.17.1 ran Ctrl-C twice during `run` live (next section); the other cases are still offline only.

### Results of the 0.17.1 release checks

0.17.1 is the test and fault-build follow-up of 0.17.0. The release binaries are `v0.17.1-rc.1` (a single
release candidate). The ordinary build behaves as in 0.17.0. The fault-injection build's `hold-post:<ms>`
now also prints `FAULT: classrun POST sending` before the classrun POST request starts (see
[Fault injection](#fault-injection)). CI on rc.1 is green on all four runners (macOS arm64, Linux x64,
Linux arm64, Windows x64). The 7.40 system was not run for 0.17.1.

**Live cell, 2026-11-25.** The new cell "Ctrl-C twice during `run`" ran from macOS arm64 on the rc.1 fault build
with the snippet `WAIT UP TO 60 SECONDS`. The first Ctrl-C comes on the POST marker, the second 15 seconds after
the waiting text. It passed on the 7.50 system over ADT HTTP, RFC (SAP GUI library) and RFC (NW RFC
SDK), and on the 758 system over the RFC SDK. Each run exited with 130 and printed one
`RUNNER_LEFTOVER: <class>` line with the two delete commands and no outcome line; `doctor` listed the
class, and the two commands removed it. Limit: that the POST had reached SAP rests on the 15 second
delay (the POST session logon measured about 1 to 2 seconds), not on an observation.

**Smoke set on the 7.50 system, rc.1.**

| Host | PASS | UNSUPPORTED | FAIL |
| :--- | ---: | ---: | ---: |
| macOS arm64 | 48 | 1 | 0 |
| Linux x64 | 48 | 3 | 0 |
| Linux arm64 | 33 | 18 | 0 |
| Windows x64, native virtual machine | 48 | 1 | 0 |

The UNSUPPORTED cells are structural: the ADT HTTP path loads no RFC library, and the OS trust store is
already installed. SAP GUI for Java runs on x86_64 only, so Linux arm64 has no RFC SAP GUI library path.
This is smoke only, not the full matrix.

### Results of the 0.18.0 release checks

The release binaries are `v0.18.0-rc.3`. rc.3 differs from rc.2 in two changes: `dump` stops on a single-byte
RFC library, and server host values are not treated as secret fragments. The runs are from 2026-11-26 and
2026-11-27. Where a cell ran more than once, the last result counts. The merged matrix has 290 scenarios in
8297 cells (4844 cells ran on rc.3, 1380 on rc.2, and 2073 UNSUPPORTED or housekeeping cells carry no binary
version).

| System | Hosts | PASS | UNSUPPORTED | FAIL |
| :--- | :--- | ---: | ---: | ---: |
| SAP 7.50 system | macOS arm64, Linux x64, Linux arm64, Windows x64 native, Windows x64 emulated on arm64 | 2933 | 519 | 4 |
| SAP 7.40 system | the same five hosts | 2015 | 1265 | 1 |
| SAP 758 system | macOS arm64, Linux x64 | 1289 | 271 | 0 |
| All | | 6237 | 2055 | 5 |

Linux arm64 has no RFC SAP GUI library path (214 cells on each of the 7.50 and 7.40 systems). The cells of
the 0.18.0 features (group m8, 648 cells) ran on rc.3: 571 PASS, 76 UNSUPPORTED and 1 FAIL.

The first runs on rc.2 and rc.3 ran several lanes of one SAP user in parallel and had 16 FAIL cells in the
merged result. On 2026-11-27 the failing cells were rerun on rc.3, one lane at a time (34 cells, all PASS),
which cleared 11 of the 16. No product defect is open. The 5 FAIL cells that remain:

- `m6-dtel-inactive` on the 7.40 system, RFC SDK, macOS arm64 (rc.2): the runner failed with several lanes of
  one SAP user in parallel (the runner cleanup proof reported that the class still existed in a new session).
  The same cell passed in the serial rerun on Linux arm64 (rc.3), on RFC through the SAP GUI library on macOS
  arm64, on the 7.50 system and on the 758 system. It was not rerun with the RFC SDK on macOS arm64.
- `object-cleanup`, `object-cleanup/transport` and `sweep-end` on the 7.50 system, macOS arm64: harness
  housekeeping cells of one run, which later runs on the system pass.
- `m8-dump-show` on the 7.50 system, RFC SDK, Windows x64 native: a harness artifact. The cell was rerun
  without its list cell (`m8-dumps-list`) and stopped with `dump-id-missing`; the full run passed.

The per-difference results are in
[Behavior differences: 0.18.0 release checks](behavior-differences.md#0180-release-checks-0180-rc2-and-rc3).

### Unsupported cells

An UNSUPPORTED cell has one of these reasons:

- **Availability.** The SAP 7.40 system holds only `$TMP` objects, so `--tr` and
  transportable-package cells are UNSUPPORTED there.
- **No ADT endpoint.** In the access path comparison, the ADT built-in endpoints were exercised for data
  element read and delete and structure read on the 7.50 system (see
  [Behavior differences](behavior-differences.md#access-path-comparison-0160-rc6)). The two data element write cells read `HTTP 400` (the comparison's request lacked a `searchHelp`
  element); the other cells read `UNSUPPORTED: no ADT
  endpoint on 7.50 or 7.40`, or `structures endpoint: only read is implemented here (write not attempted)`.
  Classrun runs these operations on both systems, so this affects only the ADT built-in path, which
  ABAPReach does not use for DDIC writes.
- **Operation not defined.** `deploy` of `TRAN` does not exist: a transaction code is created and deleted
  only (`TRAN has no deploy; use create or delete`). The ADT path has no simulate for DDIC objects.
- **Stateful session.** The SAP 7.40 system has no stateful ADT HTTP session. ADT HTTP cells for the DDIC
  types and `TRAN` are UNSUPPORTED there with the reason `ADT HTTP session is stateless on this release;
  the temporary runner class needs a stateful session (use the RFC path)`. The other ADT HTTP write cells
  run there only in their pinned and fallback form. The RFC paths cover the same scenarios.
- **Platform.** SAP GUI for Java for Linux is x86_64 only, so Linux arm64 has no SAP GUI library.
  `LD_LIBRARY_PATH` is a Linux loader variable and does not apply on macOS or Windows.
- **Console.** The prompt (`ask`) and Ctrl-C need a terminal. This applies to the interactive group
  of the 0.13.0 matrix, which the 0.15.0 matrix did not rerun. There, the emulated Windows host's
  remote execution gave the process none, so that group ran over ssh with a pseudo-terminal. The
  native Windows host ran it over a pseudo-terminal as well.
- **Fault injection.** Fault injection drops RFC replies only. ADT HTTP has no fault cell.
- **Reference comparison.** The comparison cells run the same operation through the reference tool. Its
  deploy function module is not installed on the 7.40 system, so the comparison runs on the 7.50 system
  only, except `read-table`, which passes on both systems.
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

`hold-post:<ms>` (same bounds) pauses the runner right after the classrun POST has returned, before
its outcome is mapped, and prints `FAULT: classrun POST done, pausing N ms` on stderr; it exists
only for the live Ctrl-C cells and, like `hold-lock`, drops no reply. As with `hold-lock`, the pause ends
early when Ctrl-C arrives. Right before the POST request starts it also prints
`FAULT: classrun POST sending` on stderr (the POST session may still log on after it), which the
Ctrl-C cells wait for. There is no pause when the POST session cannot be opened (the run ends in
`REJECTED: runner POST failed: cannot open the POST session` before any POST).

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
- `m6-tran-insert-failure`: a transaction code insert that fails after its `TADIR` row was written
  (the `TADIR` cleanup and `CREATE: PARTIAL`); a live run cannot force it without writing a `TSTC` row
  behind the product or racing it.
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

Median cell time in milliseconds from the rc.6 matrix on macOS arm64. The cells are the ones that
pass on all three paths of each system, so the paths run the same operations. Each RFC cell opens its
own RFC connection through SAProuter, which is most of the gap to ADT HTTP.

| System | ADT HTTP | RFC, SAP GUI library | RFC, NW RFC SDK | Cells compared per path |
| :--- | ---: | ---: | ---: | ---: |
| 7.50 system | 1028 | 1315 | 1350 | 219 |
| 7.40 system | 183 | 669 | 655 | 49 |

On the 7.40 system, ADT HTTP has fewer common cells because its HTTP session is stateless; see
[Unsupported cells](#unsupported-cells).

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

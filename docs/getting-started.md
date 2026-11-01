# Getting started

ABAPReach 0.11.1 is a macOS arm64-tested CLI for reading and deploying the active
source of **existing ABAP programs** (`PROG`). You need a Rust toolchain that
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
        "ashost": "10.0.0.15",
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
permissions. Environment variables take precedence over values in the file.
The connection file and the local credentials file must be readable by the
process. ABAPReach does not print passwords.

For a custom connections file:

```sh
export ABAP_CONNECTIONS=/path/to/connections.json
```

### TLS settings

HTTPS certificate verification is on by default. For a custom CA, add an
`adt` block to the server entry:

```text
"adt": {"scheme": "https", "ca_file": "/path/to/ca.pem"}
```

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
does not find it. Otherwise the loader checks an installed SAP GUI library on
macOS, then `SAPNWRFC_HOME`, then usual SDK folders. `doctor` also reports
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

ABAPReach 0.11.1 has no MCP server yet; it is part of 1.0.0 (see the
[roadmap](roadmap.md#mcp-server)). An AI tool may invoke the CLI as part of a
user-controlled workflow, but the CLI does not grant the AI any SAP
permissions of its own.

## Tested environment

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

- Windows (SAP GUI for Windows) and Linux (SAP GUI for Java or the NW RFC SDK).
  SAP GUI for Java ships per platform, so the Linux package and the RFC
  library inside it are checked separately when Linux is tested; the macOS
  result does not carry over.
- The `ask` prompt and a transportable deploy over ADT HTTP on 7.50: the ADT
  HTTP endpoint of that system was unreachable during these runs.
- `libsapnwrfccm.dylib`, a second RFC library that SAP GUI 8.10 revision 15
  ships next to `libsapnwrfc.dylib`; ABAPReach loads `libsapnwrfc.dylib`.

macOS on Intel is not supported: SAP GUI for Java 8.10 is the last release for
Intel Macs.

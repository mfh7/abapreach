# ABAPReach

*Let AI reach your ABAP code.*

**Read and deploy ABAP source from the command line, over ADT HTTP or RFC, including
systems you can only reach through SAProuter.**

![version](https://img.shields.io/badge/version-0.12.0-blue) ![macOS arm64](https://img.shields.io/badge/macOS_arm64-SAP_GUI_for_Java_8.10-2ea44f) ![Windows](https://img.shields.io/badge/Windows_x64-tested-2ea44f) ![Linux](https://img.shields.io/badge/Linux_x64_%7C_arm64-tested-2ea44f) ![RFC library](https://img.shields.io/badge/RFC_library-SAP_GUI_7.53_%7C_7.54_%7C_NW_RFC_SDK_7.50-0a6ed1) ![SAP_BASIS](https://img.shields.io/badge/SAP__BASIS-7.40_%7C_7.50-0a6ed1) ![backends](https://img.shields.io/badge/backends-ADT_HTTP_%7C_RFC-0a6ed1)

```text
$ abapreach deploy PROG ZDEMO ZDEMO.abap --base 63886692bf65... --conn A4H_001
backend: adt
reason: default ADT
guards: SAP lock/CSRF/transport/authorization, base hash, foreign inactive (policy reject)
not applied (FM guards): Z/Y name, productive client, system change option, SE06
--- live
+++ file
@@ -1,3 +1,3 @@
 REPORT zdemo.

-WRITE / 'v1'.
+WRITE / 'v2'.
live hash 63886692bf652fa419147f746d6b7760d6c991606fa9603d8ade3caf08ac7c64
ACTIVE
```

## Why ABAPReach

- **It works where ADT tools stop.** Many landscapes expose a system only through RFC and
  SAProuter. ABAPReach sends the same ADT requests through `SADT_REST_RFC_ENDPOINT` and keeps
  lock, write and activation on one RFC connection, so a deploy activates over RFC too.
- **One binary, nothing else to install.** No Python, no JVM, no pyrfc. For RFC it loads the
  RFC library that ships with SAP GUI, or the SAP NetWeaver RFC SDK if you have it. No SAP
  binaries are bundled.
- **It does not guess.** The live source is hashed before and after the lock. Syntax is
  checked without saving. A write whose outcome is unknown is read back and reported, never
  retried. Another developer's inactive version is not overwritten unless you allow it.
- **Same commands on both backends.** ADT HTTP is tried first. ABAPReach moves to RFC only
  when nothing has been sent yet, and never after an authorization error.

Version 0.12.0 covers existing programs (`PROG`). Every other feature of the earlier Python
tooling is scheduled for 1.0.0; see the [roadmap](#roadmap).

## Quick start

1. Build from this checkout with a Rust toolchain that supports edition 2024:

   ```sh
   cargo build --release
   ./target/release/abapreach --version
   ```

2. Describe your system in `~/.config/abap-rfc/connections.json`:

   ```json
   {
     "servers": [
       {
         "systemId": "A4H", "host": "sap.example.com", "port": 443,
         "clients": ["001"],
         "rfc": {"ashost": "sap.example.com", "sysnr": "00"}
       }
     ]
   }
   ```

   The connection ID is `<SID>_<client>`: the system ID from `systemId` and one of its
   `clients`, here `A4H_001`. Credentials use the same name with `_USER` and `_PASS`:
   `A4H_001_USER` and `A4H_001_PASS`, from the environment or from `connections.local.json`
   next to the file. Leave out `host` and `port` for an RFC-only system. Details:
   [Getting started](docs/getting-started.md#configure-a-connection); for a system behind a
   router, [Connect through SAProuter](docs/getting-started.md#connect-through-saprouter).

3. Check the connection and read a program:

   ```sh
   abapreach doctor --conn A4H_001
   abapreach read PROG ZDEMO ZDEMO.abap --conn A4H_001
   ```

4. Edit the file, simulate, then deploy with the hash `read` printed:

   ```sh
   abapreach deploy PROG ZDEMO ZDEMO.abap --simulate --base HASH_FROM_READ --conn A4H_001
   abapreach deploy PROG ZDEMO ZDEMO.abap --base HASH_FROM_READ --conn A4H_001
   ```

`doctor` probes both backends and exits 1 if either fails. An RFC failure does not stop ADT
HTTP from working; read the probe lines to see which backend is usable.

## ADT HTTP or RFC

Both backends run the same ADT operations, so a command gives the same result on either one.
They differ in what they need and where they reach.

<table>
  <tr>
    <th width="260" align="left">Operation</th>
    <th width="230" align="center"><div align="center">ADT HTTP</div></th>
    <th width="230" align="center"><div align="center">RFC</div></th>
  </tr>
  <tr>
    <td align="left"><code>read PROG</code></td>
    <td align="center">✓</td>
    <td align="center">✓</td>
  </tr>
  <tr>
    <td align="left"><code>deploy PROG --simulate</code></td>
    <td align="center">✓</td>
    <td align="center">✓</td>
  </tr>
  <tr>
    <td align="left"><code>deploy PROG</code></td>
    <td align="center">✓ ¹</td>
    <td align="center">✓</td>
  </tr>
  <tr>
    <td align="left"><code>--tr</code> check against the lock</td>
    <td align="center">✓</td>
    <td align="center">✓</td>
  </tr>
  <tr>
    <td align="left"><code>--base</code> recheck after the lock</td>
    <td align="center">✓</td>
    <td align="center">✓</td>
  </tr>
  <tr>
    <td align="left"><code>foreign_inactive</code> guard</td>
    <td align="center">✓</td>
    <td align="center">✓</td>
  </tr>
  <tr>
    <td align="left">Read-back after a timeout</td>
    <td align="center">✓</td>
    <td align="center">✓</td>
  </tr>
</table>

¹ Needs a stateful ADT session. Some 7.40 systems do not keep one; ABAPReach detects this
before it locks and deploys over RFC instead.

<table>
  <tr>
    <th width="260" align="left"></th>
    <th width="230" align="left">ADT HTTP</th>
    <th width="230" align="left">RFC</th>
  </tr>
  <tr>
    <td align="left">Client needs</td>
    <td align="left">the binary only</td>
    <td align="left">SAP GUI or NW RFC SDK library</td>
  </tr>
  <tr>
    <td align="left">Network</td>
    <td align="left">HTTPS to the ICM</td>
    <td align="left">gateway or SAProuter</td>
  </tr>
  <tr>
    <td align="left">SAP side</td>
    <td align="left">ADT services in ICF</td>
    <td align="left"><code>S_RFC</code> for <code>SADT_REST_RFC_ENDPOINT</code></td>
  </tr>
  <tr>
    <td align="left">Best for</td>
    <td align="left">direct HTTPS access</td>
    <td align="left">RFC-only landscapes</td>
  </tr>
</table>

RFC sends the ADT requests through `SADT_REST_RFC_ENDPOINT` and keeps lock, write and
activation on one connection. The RFC library is loaded at run time and is not included
in ABAPReach; installing and licensing it is up to you. Pick a backend with `--backend adt|rfc`; without it ADT HTTP runs
first, and a connection without `host` and `port` runs on RFC only.

## Platforms

Release 0.12.0 targets macOS arm64, Windows x64, Linux x64 and Linux arm64. The RFC
library lookup per OS, the release file checks, the Linux glibc baseline, the credential
file permission rules and the test matrix are in
[Getting started: Platforms and release binaries](docs/getting-started.md#platforms-and-release-binaries).
The 0.12.0 test matrix passed on every supported cell; see
[Tested environment](docs/getting-started.md#tested-environment).

An explicit `ABAPREACH_RFC_LIBRARY` that cannot be loaded stops the command with an
error. ABAPReach does not fall back to another library in that case.

## Safety

Every write goes through the same guards, on both backends.

<table>
  <tr>
    <th width="260" align="left">Guard</th>
    <th width="488" align="left">Prevents</th>
  </tr>
  <tr>
    <td align="left"><code>--base &lt;hash&gt;</code></td>
    <td align="left">overwriting a source changed since you read it</td>
  </tr>
  <tr>
    <td align="left">Syntax check without save</td>
    <td align="left">saving a source that does not compile</td>
  </tr>
  <tr>
    <td align="left">Transport request from the lock</td>
    <td align="left">writing into the wrong request</td>
  </tr>
  <tr>
    <td align="left"><code>foreign_inactive</code> policy</td>
    <td align="left">overwriting another developer's inactive version</td>
  </tr>
  <tr>
    <td align="left">Unlock on every exit</td>
    <td align="left">objects left locked after an error or Ctrl-C</td>
  </tr>
  <tr>
    <td align="left">Read-back, no retry</td>
    <td align="left">a second write after a timeout</td>
  </tr>
  <tr>
    <td align="left">No fallback after sending</td>
    <td align="left">the same write on two backends</td>
  </tr>
</table>

`--base` is checked again after the lock, in the same session. `foreign_inactive` is `reject`
by default and can be set to `ask` or `allow` per system. After a timeout the tool reads the
active and inactive source and reports what it finds. [Deploy safety](docs/deploy-safety.md)
has the details.

### Exit codes

Scripts can rely on four exit codes. Any result that needs a human look exits 1, including a
write that turned out active only after a read-back. An interrupt (Ctrl-C) exits 130; when a
deploy holds the lock, it exits 130 only after the lock is released. An interrupt that
arrives after the lock was released and the activation has started does not stop the
activation: the command reports the real result (for example `ACTIVE`, exit code 0).

<table>
  <tr>
    <th width="260" align="left">Outcome</th>
    <th width="488" align="center"><div align="center">Exit</div></th>
  </tr>
  <tr>
    <td align="left"><code>ACTIVE</code>, <code>SIMULATED_OK</code>, <code>no changes</code></td>
    <td align="center">0</td>
  </tr>
  <tr>
    <td align="left"><code>INACTIVE</code>, <code>REJECTED</code>, <code>PARTIAL</code>, <code>ACTIVE (reconciled)</code></td>
    <td align="center">1</td>
  </tr>
  <tr>
    <td align="left"><code>STOP: ...</code></td>
    <td align="center">2</td>
  </tr>
  <tr>
    <td align="left">interrupt (Ctrl-C), after the lock release when a deploy holds the lock</td>
    <td align="center">130</td>
  </tr>
</table>

## Roadmap

ABAPReach started as Python tooling (0.1.0 to 0.10.0) used in daily SAP development. The Rust
CLI ports it. 1.0.0 ships when every Python feature runs in the Rust binary, behaves the same,
and passes a live test.

<table>
  <tr>
    <th width="260" align="left">Area</th>
    <th width="488" align="left">Status</th>
  </tr>
  <tr>
    <td align="left">Programs: read, deploy, <code>doctor</code></td>
    <td align="left"><b>0.11.0</b></td>
  </tr>
  <tr>
    <td align="left">Function modules, classes, interfaces</td>
    <td align="left">planned</td>
  </tr>
  <tr>
    <td align="left">Create and delete objects</td>
    <td align="left">planned</td>
  </tr>
  <tr>
    <td align="left">DDIC, messages, text pools, CDS, enhancements</td>
    <td align="left">planned</td>
  </tr>
  <tr>
    <td align="left">Versions, restore, activation</td>
    <td align="left">planned</td>
  </tr>
  <tr>
    <td align="left">ATC, ABAP Unit, transport gate</td>
    <td align="left">planned</td>
  </tr>
  <tr>
    <td align="left">Export, drift, where-used, dumps, locks</td>
    <td align="left">planned</td>
  </tr>
  <tr>
    <td align="left">MCP server</td>
    <td align="left">planned</td>
  </tr>
  <tr>
    <td align="left">Windows x64, Linux x64 and arm64 binaries</td>
    <td align="left">planned</td>
  </tr>
</table>

Every planned row is part of 1.0.0. The full list, with the path each feature uses, is in
[docs/roadmap.md](docs/roadmap.md).

## Command reference

```text
abapreach read PROG <NAME> <OUT_FILE> [--force] --conn <SID>_<client>
abapreach deploy PROG <NAME> <FILE> [--simulate] [--base <hash>] [--tr <request>]
    [--backend adt|rfc] --conn <SID>_<client>
abapreach doctor --conn <SID>_<client>
```

`read` refuses to overwrite an existing file unless `--force` is set. `--base` and
`--backend` both pin the backend, so a failed attempt never switches to the other one.

## Documentation

- [Getting started](docs/getting-started.md): build, connection setup, first read and deploy.
- [Deploy safety](docs/deploy-safety.md): guards, policies, exit codes, recovery.
- [Behavior differences](docs/behavior-differences.md): where the Rust CLI differs from the
  Python tooling.
- [Roadmap](docs/roadmap.md): every feature on the way to 1.0.0.
- [Changelog](CHANGELOG.md).

## Development

```sh
cargo test
```

The `test-support` feature is turned on for tests by the dev-dependency, so no flag is needed.
Mutation testing of the pure logic modules uses `cargo mutants`.

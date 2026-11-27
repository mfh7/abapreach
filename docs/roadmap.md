# Roadmap

Version 1.0.0 brings every feature of the earlier internal tooling into the Rust binary, plus
four additions: running ABAP code (`run`), read-only SQL with joins, object search, and the
application server instance in `doctor` and ping. That scope is fixed: no other feature is added,
and no feature of the earlier tooling is dropped. Each milestone ships as a minor
release (0.12.0, 0.13.0, ...); 1.0.0 follows when every feature below is released and the
[release gate](#release-gate) passes. Shipped work moves to the [changelog](../CHANGELOG.md).

## Milestones

| Version | Scope |
| :--- | :--- |
| 0.11.0 | Programs: read and deploy over ADT HTTP and RFC, `doctor` (released) |
| 0.11.1 | Fixes from the three-path live test, SAProuter route outside the shared config (released) |
| 0.12.0 | Windows and Linux: RFC library lookup per platform, live tests on every path, CI builds (released) |
| 0.13.0 | Function modules, classes with local includes, interfaces: read and deploy; create of function groups, function modules, classes and interfaces; read with line ranges (released) |
| 0.14.0 | MCP server (stdio), `read-table`, `delete` (released) |
| 0.15.0 | Programs and includes: `create PROG` (program and include), include read, deploy and delete; ADT HTTP logoff fix; MCP additions (released) |
| 0.16.0 | DDIC: data elements, domains, tables (transparent tables and structures), table types; transaction codes (`TRAN`): read, simulate, create and delete, through classrun on the 7.50 and 7.40 systems (released) |
| 0.16.1 | `--tr` for objects no open request holds; classrun on newer releases (tested on a S/4HANA 758 system); classrun POST from a new session; runner cleanup proof against `TADIR`; MCP re-reads connections per call (released) |
| 0.17.0 | `inactive`, `activate`, `aunit` (ABAP Unit) and `run` (class, report with ALV capture, code snippet), with MCP tools; defaults: `run: plan`, `mcp_run: off`, `mcp_run_snippet: off`; tested on the 7.50, 7.40 and S/4HANA 758 systems (released) |
| 0.17.1 | Test and fault-build follow-ups of 0.17.0: a live cell for Ctrl-C twice during `run`, and a `FAULT: classrun POST sending` marker in the fault-injection build; the ordinary build is unchanged (released) |
| 0.18.0 | MCP deploy by `edits` and `base`, with the whole-source hash on a partial read; `sql` (read-only SELECT with joins), `search`, `where-used`, `dumps` and `dump`, `ping` and the instance line in `doctor`; `read-table` checks views and refuses with exit 2 |
| later | The remaining features below, one milestone at a time |

Platforms come before new features, so every feature after 0.12.0 is built and tested on
macOS, Windows and Linux from the start.

## How a feature reaches SAP

Each feature runs on at least one of three paths. Where more than one fits, ADT HTTP is tried
first.

| Path | Needs | Used for |
| :--- | :--- | :--- |
| **ADT** | HTTPS, or an RFC library for the ADT tunnel | everything ADT offers |
| **ADT?** | same as ADT | ADT if live tests confirm the object type |
| **classrun** | same as ADT | a temporary class created over ADT and run through `/sap/bc/adt/oo/classrun` calls standard function modules on the server, then is deleted; for objects ADT has no endpoint for. It needs developer authorization for a temporary `$TMP` class; nothing is installed on the SAP system |
| **standard?** | depends on the path found | offered only if its milestone proves a standard SAP path (ADT, classrun, or a remote-enabled standard function module); otherwise not supported |

**ADT** covers both ADT HTTP and the RFC tunnel through `SADT_REST_RFC_ENDPOINT`; neither
needs anything installed in SAP.

The access paths were compared for the DDIC types on the 7.50 and 7.40 systems; the results are in
[Behavior differences: Access path comparison](behavior-differences.md#access-path-comparison-0160-rc6).

## Released in 0.11.0

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Read and deploy `PROG` | `rfc-deploy PROG` | ADT |
| `doctor` | new | ADT, RFC |
| `foreign_inactive` guard | function module verdict | ADT |

## Shipped in 0.13.0

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Read with line ranges (`--lines`, stdout only) | `rfc-read source` | ADT |
| Function modules, with interface-change report and where-used advisory | `rfc-deploy FUNC` | ADT |
| Classes, including local includes `CCIMP`, `CCDEF`, `CCMAC` and `CCAU` | `rfc-deploy CLAS` | ADT |
| Interfaces | `rfc-deploy INTF` | ADT |
| Create function groups, function modules, classes and interfaces | `rfc-deploy create` | ADT |

The status of each path is in [Getting started: Tested environment](getting-started.md#tested-environment),
and the intentional differences from the reference behavior are in
[Behavior differences](behavior-differences.md).

## Shipped in 0.16.0

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Data elements (`DTEL`), domains (`DOMA`), transparent tables and structures (`TABL`), table types (`TTYP`): read, simulate, deploy, create, delete | `rfc-read`, `rfc-deploy` DDIC commands | classrun on 7.50 and 7.40 |
| Transaction codes (`TRAN`) for report programs: read, create, delete (no deploy) | `rfc-read`, `rfc-deploy` transaction commands | classrun on 7.50 and 7.40 |
| MCP: the DDIC and `TRAN` types; `read` of these types takes the write slot and is refused under `mcp_write = off` | MCP server (0.14.0) | stdio |

DDIC changes are decided like SAP activation: a change that needs SE14 conversion is refused,
and a table that holds data is not deleted. The file size limit is 256 KiB.

Each of these runs a temporary class in `$TMP`, so every operation needs a modifiable system and
developer authorization. Setup and limits: [Getting started: DDIC objects and transaction codes](getting-started.md#ddic-objects-and-transaction-codes),
[Deploy safety: DDIC and TRAN](deploy-safety.md#ddic-and-tran), and [MCP server](mcp.md#ddic-and-tran-types).

Not in 0.16.0: messages (`MSAG`), text pools (`TEXT`), CDS (`DDLS`), enhancements (`ENHO`), restore
of an old version (`rfc-deploy restore`), and the lifecycle (inactive, activate, ATC, ABAP Unit).
Deploy of `TRAN` does not exist: a transaction code is created and deleted, not changed.
Inactive objects, activation and ABAP Unit shipped in 0.17.0; ATC is still planned.

## Shipped in 0.17.0

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| List the inactive objects of the logon user (`inactive [--all]`) | `inactive` | ADT |
| Activate own inactive Z/Y objects (`activate`, with `--simulate`) | `activate` | ADT |
| ABAP Unit for a program, class or function group, or for a package or request (`aunit`) | `aunit` | ADT |
| Run an existing class that implements `IF_OO_ADT_CLASSRUN` (`run CLAS`) | new | classrun |
| Run an executable report and print its list, with ALV capture (`run PROG`) | new | classrun (temporary runner class) |
| Run a code snippet (`run SNIPPET`) | new | classrun (temporary runner class) |
| MCP: `abap_inactive`, `abap_activate_simulate`, `abap_activate`, `abap_aunit`, `abap_run` | MCP server (0.14.0) | stdio |

`run` executes code on the SAP system, so it has its own safety gates. The policy key `run`
defaults to `plan`: a run needs the plan file from `run --simulate`. A productive client refuses
every run, and `run PROG` and `run SNIPPET` work on development systems only. In MCP, `abap_run` and
`abap_aunit` stay off until a connection sets `mcp_run: confirm` and lists each object in
`mcp_run_allow`; a snippet also needs `mcp_run_snippet: on`. The human confirms every MCP run.
The reply shapes of `inactive` and `run` were checked live on the 7.50, 7.40 and S/4HANA 758 systems.
The behavior, outcomes and limits of each command are in the [changelog](../CHANGELOG.md) and
[MCP server](mcp.md).

Not in 0.17.0: ATC (`atc`).

## Shipped in 0.18.0

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Read-only SQL with joins, a closed grammar (`sql`) | new | ADT freestyle data preview |
| Object search by name pattern and type (`search`) | new | ADT |
| Where-used (`where-used`), ADT first and the cross-reference tables as fallback | `rfc-read where-used` | ADT, standard tables |
| Short dumps (`dumps`, `dump`) | `rfc-read dumps` | data preview of `SNAP`, `RFC_READ_TABLE` on the 7.50 system |
| Ping, system information and the application server instance (`ping`, and the instance line of `doctor`) | `rfc-read ping` | ADT, RFC |
| MCP: deploy by `edits` and `base`, the whole-source `hash` of a partial read, the secret guard on every deploy | MCP server (0.14.0) | stdio |
| MCP: `abap_sql`, `abap_search`, `abap_where_used`, `abap_dumps`, `abap_dump`, `abap_ping` | MCP server (0.14.0) | stdio |

All of them are read-only and use standard SAP paths. `sql` keeps the `read-table` policies (denied
tables, row limits) and fails closed on views: a view is read only when its base tables are all
allowed. `read-table` has the same check, and a dictionary refusal there is now `STOP` with exit 2.
In MCP, every deploy is refused when the live source holds a configured secret. Where-used for
`MSAG` is planned for 0.19.0. The behavior, the limits per release and the differences from the
earlier tooling are in [Getting started](getting-started.md), [MCP server](mcp.md),
[Behavior differences](behavior-differences.md) and the [changelog](../CHANGELOG.md).

## Planned for 1.0.0

### Other object types

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Message classes, text pools | `MSAG` `TEXT` | ADT? |
| CDS views | `DDLS` | ADT? |
| Enhancement implementations | `ENHO` | ADT? |

### Lifecycle and quality

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Version list and diff | `rfc-read versions` | ADT? |
| Restore an old version | `rfc-deploy restore` | standard? |
| Transport gate | `rfc-gate` | standard? |
| Package export and drift | `export`, `drift` | standard? |
| Own locks (SM12), unlock | `locks`, `unlock` | standard? |
| ATC | `atc` | ADT |

### SAP_BASIS 758 follow-ups

| Feature | Note |
| :--- | :--- |
| TABL conversion check on 758 | 758 has no `DD_DECIDE`, so a TABL change that needs the conversion verdict is rejected. The class `CL_DD_DECIDE` exists on 758 and may replace it; its API is unverified and needs a design note. |

### System information

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| DDIC field info | `rfc-read ddic` | RFC (`DDIF_FIELDINFO_GET` is remote-enabled) |
| Test call of a remote FM | `rfc-call` | RFC |

### Additions beyond the earlier tooling

The four additions have shipped: `run` in 0.17.0, and `sql`, `search` and the application server
instance in `doctor` and `ping` in 0.18.0. No addition is left.

### Outside the 1.0.0 scope, timing to be decided

Features ABAP FS offers that are not part of 1.0.0. Whether each comes before or after 1.0.0 is
decided later.

| Feature | Note |
| :--- | :--- |
| ABAP debugger (breakpoints, stepping, variables) | ADT debugger API |
| Transport view (browse, compare, release requests) | release stays a user action today |
| Blame (who changed each line, when, under which request) | needs version history (`versions`, planned for 1.0.0) |
| Compare one object between two systems | needs two connections in one command |

### Setup

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| `doctor`: installed objects, capabilities | new | ADT, RFC |

## Shipped in 0.14.0

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Read a table through the data preview (`read-table`) | `rfc-read table` | ADT, RFC SADT for the admitted shape |
| Delete with a reviewed plan (`delete`) | `rfc-deploy delete` | ADT, RFC SADT fallback before the first LOCK |
| MCP server (`abapreach mcp`): read tools, write tools behind `mcp_write` | MCP server (earlier tooling) | stdio |

Setup and limits: [MCP server](mcp.md) and [Deploy safety: Delete](deploy-safety.md#delete).

## Shipped in 0.15.0

| Feature | Reference command | Path |
| :--- | :--- | :--- |
| Create a program (`create PROG --subc 1`), with an optional FILE that runs a simulate deploy | `rfc-deploy create PROG` | ADT, RFC SADT |
| Create an include (`create PROG --subc I`); read, deploy (`--main`) and delete of program includes | `rfc-deploy create PROG` (include create parity only; the other include operations are not in the parity matrix) | ADT, RFC SADT |
| MCP: `subc` and `main` arguments; plan tokens bound to their arguments and backend | MCP server (0.14.0) | stdio |

Setup and limits: [Getting started: Program includes](getting-started.md#program-includes),
[Deploy safety](deploy-safety.md), and [MCP server](mcp.md).

Not in 0.15.0: DDIC (`DTEL`, `DOMA`, `TABL`, `TTYP`), messages (`MSAG`), text pools (`TEXT`), CDS
(`DDLS`), enhancements (`ENHO`), restore of an old version (`rfc-deploy restore`), and the
lifecycle (inactive, activate, ATC, ABAP Unit). DDIC and `TRAN` shipped in 0.16.0.

## MCP server (0.14.0)

`abapreach mcp` serves the tools over stdio. Each MCP tool maps to one CLI operation with the
same guards. Writes need a `plan_token` from a simulate, and `mcp_write` decides whether a human
must also confirm. The tool list is in [MCP server](mcp.md#tools).

Not yet in 0.14.0: Codex client support (not verified in this release); restore of an old version
(`rfc-deploy restore`); creation of programs and includes.

## Platforms and distribution

Each platform is tested on every path it offers. A path SAP does not ship for a platform is
listed as not available.

| Platform | ADT HTTP | RFC, SAP GUI library | RFC, NW RFC SDK | Status |
| :--- | :--- | :--- | :--- | :--- |
| macOS arm64 | yes | yes | yes | **0.11.0**, tested live in 0.11.1 |
| Windows x64 | yes | yes | yes | **0.12.0**, tested live |
| Linux x64 | yes | yes | yes | **0.12.0**, tested live |
| Linux arm64 | yes | not available ¹ | yes | **0.12.0**, tested live |

¹ SAP GUI for Java for Linux is built for x86_64 only.

SAP GUI for Windows and its RFC library are x64 only, so the Windows build is x64; on
Windows on ARM it runs under x64 emulation together with SAP GUI. macOS on Intel is not
planned: SAP GUI for Java 8.10 is the last release for Intel Macs. CI builds and tests every
platform above and attaches the binaries to each release. A platform whose RFC path fails its
live test is released as ADT HTTP only and documented that way.

## Release gate

1.0.0 is published only when all of the following hold:

1. The license terms for using the SAP GUI RFC library and the NW RFC SDK from a third-party
   program are confirmed from SAP's own text, for each platform that claims RFC.
2. Every row of the feature table runs on at least one path and passes the comparison against
   the reference behavior: same status, exit code and SAP state, same guards.
3. The final ADT HTTP versus RFC comparison is published in the README.
4. The public repository starts from a clean snapshot.

## Not planned

- Feature parity with other ADT command-line tools (refactoring, Clean Core checks, OData).
- Bundling SAP binaries or reimplementing the RFC protocol.
- HTTP over SAProuter and SOAP RFC, until their feasibility and security are proven.
- Single sign-on for ADT HTTP (SAML, X.509, Kerberos) in 1.0.0.
- Atomic rollback across several objects.

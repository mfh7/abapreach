# Roadmap

Version 1.0.0 brings every feature of the earlier Python tooling into the Rust binary. That
scope is fixed: features are not added to match other tools, and none of the Python features
is dropped. Each milestone ships as a minor release (0.12.0, 0.13.0, ...); 1.0.0 follows when
every feature below is released and the [release gate](#release-gate) passes. Shipped work
moves to the [changelog](../CHANGELOG.md).

## Milestones

| Version | Scope |
| :--- | :--- |
| 0.11.0 | Programs: read and deploy over ADT HTTP and RFC, `doctor` (released) |
| 0.11.1 | Fixes from the three-path live test, SAProuter route outside the shared config (released) |
| 0.12.0 | Windows and Linux: RFC library lookup per platform, live tests on every path, CI builds (released) |
| 0.13.0 | Function modules, classes with local includes, interfaces: read and deploy; create of function groups, function modules, classes and interfaces; read with line ranges |
| 0.14.0 | MCP server |
| later | The remaining features below, one milestone at a time |

Platforms come before new features, so every feature after 0.12.0 is built and tested on
macOS, Windows and Linux from the start.

## How a feature reaches SAP

Each feature runs on at least one of three paths. Where more than one fits, ADT HTTP is tried
first.

| Path | Needs | Used for |
| :--- | :--- | :--- |
| **ADT** | HTTPS, or an RFC library for the ADT tunnel | everything ADT offers |
| **ADT?** | same as ADT | ADT if live tests confirm the object type, else **EXT** |
| **EXT** | RFC library and ABAPReach's ABAP objects | operations ADT does not offer |

**ADT** covers both ADT HTTP and the RFC tunnel through `SADT_REST_RFC_ENDPOINT`; neither
needs anything installed in SAP. **EXT** objects are installed with `abapreach install`,
abapGit, or by hand. An **EXT** operation on a system without them stops with a message that
names `install`.

## Released in 0.11.0

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| Read and deploy `PROG` | `rfc-deploy PROG` | ADT |
| `doctor` | new | ADT, RFC |
| `foreign_inactive` guard | FM verdict | ADT |

## Shipped in 0.13.0

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| Read with line ranges (`--lines`, stdout only) | `rfc-read source` | ADT |
| Function modules, with interface-change report and where-used advisory | `rfc-deploy FUNC` | ADT |
| Classes, including local includes `CCIMP`, `CCDEF`, `CCMAC` and `CCAU` | `rfc-deploy CLAS` | ADT |
| Interfaces | `rfc-deploy INTF` | ADT |
| Create function groups, function modules, classes and interfaces | `rfc-deploy create` | ADT |

The status of each path is in [Getting started: Tested environment](getting-started.md#tested-environment),
and the intentional differences from the Python tooling are in
[Behavior differences](behavior-differences.md).

## Planned for 1.0.0

### Source objects

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| Create programs and program includes | `rfc-deploy create` | ADT |
| Create report transactions | `create TRAN` | ADT? |

### Other object types

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| Data elements, domains, tables, table types | `DTEL` `DOMA` `TABL` `TTYP` | ADT? |
| Message classes, text pools | `MSAG` `TEXT` | ADT? |
| CDS views | `DDLS` | ADT? |
| Enhancement implementations | `ENHO` | ADT? |

DDIC changes are decided like SAP activation: a change that needs SE14 conversion is refused,
and a table that holds data is not deleted.

### Lifecycle and quality

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| Delete with a reviewed plan | `rfc-deploy delete` | EXT |
| Version list and diff | `rfc-read versions` | ADT? |
| Restore an old version | `rfc-deploy restore` | EXT |
| Inactive objects, activation | `inactive`, `activate` | ADT |
| ATC, ABAP Unit | `atc`, `aunit` | ADT |
| Transport gate | `rfc-gate` | EXT |

### System information

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| Ping and system info | `rfc-read ping` | ADT, RFC |
| Package export and drift | `export`, `drift` | EXT |
| Own locks (SM12), unlock | `locks`, `unlock` | EXT |
| Table read with denylist | `rfc-read table` | EXT |
| DDIC field info | `rfc-read ddic` | EXT |
| Short dumps (ST22) | `rfc-read dumps` | ADT? |
| Where-used | `rfc-read where-used` | ADT? |
| Test call of a remote FM | `rfc-call` | RFC |

### Setup

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| `install` of the EXT objects | new | ADT |
| `doctor`: installed objects, capabilities | new | ADT, RFC |

## MCP server

The same binary will run as an MCP server over stdio, so AI assistants can use ABAPReach.
Each MCP tool maps to one CLI operation with the same guards. Writes keep the gate of the
Python MCP server: a simulate returns a plan token, and only a write that presents that token
runs. The Python server offered `read_source`, `read_table`, `simulate`, `deploy`, `create`,
`create_simulate`, `delete` and `delete_simulate`; all of them return in Rust.

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
2. Every row of the feature table runs on at least one path and passes the parity test
   against the Python tooling: same status, exit code and SAP state, same guards.
3. The final ADT HTTP versus RFC comparison is published in the README.
4. The public repository starts from a clean snapshot.
5. The Python tooling is archived with documentation that credits its role.

## Not planned

- Feature parity with other ADT command-line tools (refactoring, Clean Core checks, OData).
- Bundling SAP binaries or reimplementing the RFC protocol.
- HTTP over SAProuter and SOAP RFC, until their feasibility and security are proven.
- Single sign-on for ADT HTTP (SAML, X.509, Kerberos) in 1.0.0.
- Atomic rollback across several objects.

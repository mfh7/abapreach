# Roadmap

Version 1.0.0 brings every feature of the earlier Python tooling into the Rust binary. That
scope is fixed: features are not added to match other tools, and none of the Python features
is dropped. Each milestone ships as a minor release (0.12.0, 0.13.0, ...); 1.0.0 follows when
every feature below is released and the [release gate](#release-gate) passes. Shipped work
moves to the [changelog](../CHANGELOG.md).

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

## Planned for 1.0.0

### Source objects

| Feature | Python origin | Path |
| :--- | :--- | :--- |
| Read with line ranges | `rfc-read source` | ADT |
| Function modules, interface changes | `rfc-deploy FUNC` | ADT |
| Classes and local includes | `rfc-deploy CLAS` | ADT |
| Interfaces | `rfc-deploy INTF` | ADT |
| Create programs, includes, groups, FMs, classes | `rfc-deploy create` | ADT |
| Create report transactions | `create TRAN` | ADT? |

Deploying a function module with a changed interface lists the callers found by where-used.

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

| Platform | Status |
| :--- | :--- |
| macOS arm64 | **0.11.0**, tested live |
| Windows x64 | planned |
| Linux x64 | planned |
| CI release binaries | planned |

macOS arm64 was tested with the RFC library of SAP GUI for Java. Windows uses the SAP GUI
library; Linux uses the SAP GUI for Java library or the NW RFC SDK. A platform whose RFC path
fails its live test is released as ADT HTTP only and documented that way.

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

# M9 test fixtures

Scrubbed SAP ADT reply captures for the M9 edits and read features.

## Source

- Captured 2026-11-25, read-only, from three test systems:
  - `750`: HD1 (SAP release 750, raw connection `hd1_110`)
  - `740`: FED (SAP release 740, raw connection `fed_100`)
  - `758`: S4D (SAP release 758, raw connection `s4d_100`)
- File name: `<kind>-<release>.xml`. Non-200 replies carry the HTTP status as a suffix (`-400`, `-404`).
- Endpoint per kind:
  - `usage-*`: ADT where-used (`usageReferences` result).
  - `sysinfo-*`: `/sap/bc/adt/system/information`.
  - `search-*`: ADT object search.
  - `snap-*`: ADT data preview of table SNAP.
  - `dd02l-entity`, `dd26s-view`, `ddldependency`, `freestyle-join`: ADT data preview and DDIC/CDS replies.
- Status:
  - `snap-list-750-400.xml` and `snap-freestyle-750-400.xml`: HTTP 400 (HD1 refuses SNAP with standard tools).
  - `sysinfo-740-404.xml`: HTTP 404. Plain text, not XML. Kept as captured.
  - `usage-func-750.xml`: HTTP 200 on retry.

## Trimmed files

- Files over 20 KB were trimmed to the first 5 `usageReferences:referencedObject` elements:
  - `usage-clas-740.xml` (from 218,849 bytes)
  - `usage-func-740.xml` (from 494,726 bytes)
  - `usage-clas-750.xml` (from 113,500 bytes)
  - `usage-func-750.xml` (from 433,342 bytes)
- The root element and its attributes are unchanged, including `numberOfResults`.
- `usage-clas-758.xml` (19,931 bytes) is under the limit and is not trimmed.

## Scrub rules

- Hostnames: the source application server host names become `apphost`. The source database host name becomes `dbhost`.
  - Application server names keep the SID and instance number, for example `apphost_HD1_00`.
- IP addresses (dotted quads) become `192.0.2.1`.
- SNAP rows: `UNAME` values become `TESTUSER`. `AHOST` values become `apphost`.
- FLIST payloads keep their tag and length structure. No user or host was found inside them.
- Every non-SAP Z/Y object name in usage replies becomes `ZTEST_OBJECT`.
- Responsible user names (personal developer IDs) become `TESTUSER`.
- Kept as they are: SAP standard object names, SIDs (HD1, FED, S4D), releases, and responsible `SAP`.

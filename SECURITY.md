# Security policy

ABAPReach handles SAP credentials and changes objects in SAP systems, so a
security issue can be serious. Please report it privately, not in a public issue.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: open the repository's **Security**
tab and choose **Report a vulnerability**. Include the version (`abapreach
--version`), the platform, the steps to reproduce, and what an attacker could do.
Do not include real hosts, user names, passwords or SAProuter routes; use
placeholders.

You get a first answer within seven days. A confirmed issue is fixed in a new
release, and the advisory is published once the fix is available.

## Supported versions

Only the latest release receives security fixes.

## Scope

In scope: the `abapreach` binary and its MCP server, the release archives, and
the documentation where it gives security-relevant instructions (for example the
permissions of `connections.local.json`).

Out of scope: the SAP systems themselves, SAP GUI and the SAP NW RFC SDK. Report
issues in those to SAP.

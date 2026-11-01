# Contributing

## Branches

Never commit to `main` directly. Every change goes on its own branch, named
`<type>/<short-description>` in lower-case kebab case, and reaches `main`
through a pull request.

| Type | Use for |
| :--- | :--- |
| `feat` | a new command, option or capability |
| `fix` | a bug fix |
| `docs` | documentation only |
| `test` | tests only |
| `refactor` | code changes that keep behavior |
| `perf` | faster or leaner, same behavior |
| `build` | `Cargo.toml`, toolchain, dependencies |
| `ci` | CI workflows |
| `chore` | anything else that does not change behavior |

Examples: `feat/deploy-func`, `fix/connect-error-fallback`,
`docs/getting-started-saprouter`.

## Commits

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
`<type>(<scope>): <summary>`, with the same types as branches. A branch may
hold commits of other types when they belong to the same change.

## Before a pull request

- `cargo test` passes.
- Behavior changes are tested on every path: ADT HTTP, RFC with the SAP GUI
  library, and RFC with the NW RFC SDK. A case a path cannot support or test
  is written down with the reason.
- User-facing changes are recorded under `[Unreleased]` in `CHANGELOG.md`.
- Product text, messages, comments and identifiers are in English, and the
  repository holds no details of a real SAP landscape (hosts, SIDs, users,
  transport requests).

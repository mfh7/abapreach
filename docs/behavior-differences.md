# Behavior differences from the Python tooling

Only differences that are intended are listed.

## Inactive version after a failed activation

The Python function module (`YFM_UTILS_SOURCE_DEPLOY`) discards the inactive version when the
activation fails. The ADT paths of `abapreach` (HTTP and RFC/SADT) do not: after a failed
activation the failed source stays as the inactive version of the program (an entry in
`DWINACTIV`), the active source is unchanged, and the status is `INACTIVE` with exit 1. This is
the only intended difference for `PROG`.

## Related behavior that is not a difference

- `deploy --simulate` writes nothing in both tools (a syntax check only); the active hash, the
  inactive entries and the enqueue table are equal after a simulate.
- `--base` is the plain content hash of `GET <source>?version=active`, computed the same way
  on both Rust backends. Whether a hash from the Python tooling is accepted is decided by the
  live comparison in the oracle test; a mismatch is recorded here.
- `GET <source>?version=inactive` without an inactive version answers 200 with the active
  source, so the reconciliation decides by content and never by a status code.

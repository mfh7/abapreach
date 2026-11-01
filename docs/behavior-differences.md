# Behavior differences from the Python tooling

Only differences that are intended are listed.

## Inactive version after a failed activation

The Python function module (`YFM_UTILS_SOURCE_DEPLOY`) discards the inactive version when the
activation fails. The ADT paths of `abapreach` (HTTP and RFC/SADT) do not: after a failed
activation the failed source stays as the inactive version of the program (an entry in
`DWINACTIV`), the active source is unchanged, and the status is `INACTIVE` with exit 1. This is
the only intended difference for `PROG`.

## A transportable object whose lock reports no transport request is refused

When the lock of a transportable object (package not `$TMP`) answers 200 without a `CORRNR`,
`abapreach` stops with `STOP: the object is not local but the lock returned no transport request`
(exit 2) after releasing the lock and before the syntax check and the `PUT`. This holds even when
`--tr` is given: an empty `CORRNR` is never replaced by `--tr`.

The Python tooling would still write there. For a transportable object it requires `--tr`, checks
that the request is a modifiable workbench request owned by the user (`E070`) and that the object
is not enqueued in another request (`TLOCK`), then passes `--tr` as `IV_TRKORR`; the function module
locks with exactly that request (`set_lifecycle_corr`: `mo_lm->corrnr = mv_trkorr`). The request of
the write is therefore chosen by the client, not read from the object lock.

`abapreach` stays strict on purpose: on the ADT path the `CORRNR` of the lock response is the only
statement SAP makes about the request that owns the object lock, so with an empty `CORRNR` the tool
cannot prove that the `--tr` request owns the object. Writing into a request that merely accepts
the object is not the same as writing into the request that owns its lock, and the tool refuses
rather than guesses; nothing is written and no lock is left behind. To deploy such an object, give
it a request on the SAP side (or pass a request the lock reports) and run the deploy again.

## Related behavior that is not a difference

- An inactive version of another user: with the default policy `foreign_inactive: reject`
  `abapreach` refuses the deploy before the lock (`REJECTED: inactive version of another user
  exists: <OWNER>`, exit 1) and writes nothing, which is the verdict of the Python function
  module ("Inactive version of another user exists"). The same list is read again after the
  lock, so a version that appears in between is refused there too and the lock is released. The
  policies `allow` and `ask` exist only in `abapreach`, so the default stays at parity with
  Python.
- `deploy --simulate` writes nothing in both tools (a syntax check only); the active hash, the
  inactive entries and the enqueue table are equal after a simulate.
- `--base` is the plain content hash of `GET <source>?version=active`, computed the same way
  on both Rust backends. Whether a hash from the Python tooling is accepted is decided by the
  live comparison in the oracle test; a mismatch is recorded here.
- `GET <source>?version=inactive` without an inactive version answers 200 with the active
  source, so the reconciliation decides by content and never by a status code.

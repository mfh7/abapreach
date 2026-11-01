# Behavior differences from the Python tooling

The Rust CLI is a partial port of the earlier Python tooling. This page
lists known intentional behavior differences for operations available in
the Rust CLI. It is not a feature-parity claim. See the [roadmap](roadmap.md)
for current scope.

## Inactive version after a failed activation

The Python function module (`YFM_UTILS_SOURCE_DEPLOY`) discards the inactive
version when activation fails. The ADT paths of `abapreach` (HTTP and
RFC/SADT) do not: after a failed activation, the failed source remains as
the program's inactive version (an entry in `DWINACTIV`), the active source
is unchanged, and the status is `INACTIVE` with exit 1. Inspect the inactive
version before attempting another deploy.

## A transportable object whose lock reports no transport request is refused

When the lock of a transportable object (package not `$TMP`) answers 200
without a `CORRNR`, `abapreach` stops with `STOP: the object is not local but
the lock returned no transport request` (exit 2) after releasing the lock
and before the syntax check and PUT. This holds even when `--tr` is given:
an empty `CORRNR` is never replaced by `--tr`.

The Python tooling would still write there. For a transportable object it
requires `--tr`, checks that the request is a modifiable workbench request
owned by the user (`E070`) and that the object is not enqueued in another
request (`TLOCK`), then passes `--tr` as `IV_TRKORR`; the function module
locks with exactly that request (`set_lifecycle_corr`: `mo_lm->corrnr =
mv_trkorr`). The request of the write is therefore chosen by the client,
not read from the object lock.

`abapreach` stays strict on purpose: on the ADT path, the `CORRNR` of the
lock response is the only statement SAP makes about the request that owns
the object lock. With an empty `CORRNR`, the tool cannot prove that the
`--tr` request owns the object. It refuses before writing and releases the
lock. Assign the object to a transport request on the SAP side, then retry
only after verifying the lock response.

## Related behavior and comparison limits

- With the default `foreign_inactive: reject` policy, `abapreach` refuses
  another user's inactive version, matching the Python function module's
  verdict. The Python tooling always refuses; the `allow` and `ask`
  policies exist only in `abapreach`.
- `deploy --simulate` writes nothing in both tools (a syntax check only);
  the active hash, inactive entries, and enqueue table are equal afterward.
- Both Rust backends compute `--base` from the normalized active source, the
  same way as the Python tooling. A live comparison on SAP_BASIS 7.50 gave
  the same source and the same hash from Rust `read` and from the Python
  function module, so a hash from either tool is accepted by the other.
- `GET <source>?version=inactive` without an inactive version can answer
  200 with the active source, so reconciliation compares content rather than
  relying on that HTTP status alone.

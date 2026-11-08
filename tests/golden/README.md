# Golden files of the public command line and MCP schema

`old/` holds the output of a build of `f7881c8`, the last commit of the old M5 branch (the one that
still had `TRAN` and the function-module EXT layer): `--help` of the top level and of every
subcommand, `--version`, the grammar errors of an unknown type, and the MCP `tools/list` schema
(`mcp-tools-write.json` with the write tools offered, `mcp-tools-read.json` without).

`new/` holds what the public binary prints without any extension. `tests/golden_test.rs` runs the
binary against `new/` and checks the rule below against `old/`.

## The rule

`new/` is `old/` minus `TRAN`, `--prog` and `prog`, as a plain deletion:

- text files: drop every line that names `--prog`, and drop `, TRAN` and `|TRAN` from the lines
  that remain;
- JSON files: drop the `prog` property, drop `"TRAN"` from every `enum`, drop `|TRAN` from a
  description.

## The one exception: `help-read.txt` and `help-deploy.txt`

In `old/`, `TRAN` was the only possible value of the type argument that carried help text. clap
prints the long `--help` of a type argument with at least one value that has help text as a
`Possible values:` block, and every other argument in the same wide layout. With `TRAN` gone no
value has help text, so clap prints the inline `[possible values: PROG, FUNC, CLAS, INTF]` form
and the compact layout of the other subcommands. That layout change is not a deletion of lines,
so for these two files the golden is the inline form, and the test checks that the same options,
help sentences and possible values are still there.

This refinement of "identical minus `TRAN`" is a decision of the project manager, not of the user
(plan task S2).

## Since M6 Task 17

The DDIC types (`DTEL`, `DOMA`, `TABL`, `TTYP`) and `TRAN` are types of the public command line
again, and `--prog` is an option of `create`. The text files of `new/` that mention the types
(`help-read`, `help-deploy`, `help-create`, `help-delete` and the four `err-*` files) therefore
follow the M6 contract and no longer relate to `old/` by the rule above; only `help-top.txt` and
the MCP schemas do, until Task 26 moves the MCP schemas as well. `tests/golden_test.rs` checks the
M6 texts directly.

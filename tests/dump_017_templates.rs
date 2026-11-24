//! Writes the generated runner source of the 0.17 run modes to files for a compile check on a
//! real system (paste or deploy it by hand). Nothing here talks to SAP, and the test is ignored:
//! it is a tool, not a check.
//!
//! ```text
//! cargo test --test dump_017_templates -- --ignored --nocapture
//! ```
//!
//! The files go to `$ABAPREACH_TEMPLATE_DUMP` (default
//! `~/VSCode/ABAP/.devrun/017/templates`), one per class and `OUT` type:
//! `<CLASS>-<OUTTYPE>.abap`. The nonce, the payload values and the expiry are fixed, so the output
//! is the same on every run.

use abapreach::cli::{AlvColumns, AlvMode};
use abapreach::operation::payload::{RunProg, RunRequest, encode_run};
use abapreach::operation::runner_template::{
    OutType, RUN_PROG_TEMPLATE, generate_for, generate_snippet,
};
use std::fs;
use std::path::PathBuf;

const PROG_CLASS: &str = "YTEST_ABAPREACH_P17P";
const SNIPPET_CLASS: &str = "YTEST_ABAPREACH_P17S";
const NONCE: &str = "0123456789ABCDEF0123456789ABCDEF";
const BASE_HASH: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const SNIPPET: &str = "DATA(lv_text) = |hello { sy-uname }|.\nprint( lv_text ).";

fn directory() -> PathBuf {
    std::env::var_os("ABAPREACH_TEMPLATE_DUMP").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set"))
                .join("VSCode/ABAP/.devrun/017/templates")
        },
        PathBuf::from,
    )
}

#[test]
#[ignore = "writes the templates of the 0.17 compile check; run it by hand"]
fn dump_017_templates() {
    let params = [("P_DATE".to_owned(), "20261101".to_owned())];
    let prog_payload = encode_run(&RunRequest {
        nonce: NONCE,
        user: "DEVUSER",
        expires: "20991231235959",
        max_bytes: 1 << 20,
        prog: Some(RunProg {
            report: "ZABAPREACH_REPORT",
            base: BASE_HASH,
            params: &params,
            variant: None,
            alv: AlvMode::On,
            alv_columns: AlvColumns::Visible,
        }),
    })
    .unwrap();
    let snippet_payload = encode_run(&RunRequest {
        nonce: NONCE,
        user: "DEVUSER",
        expires: "20991231235959",
        max_bytes: 1 << 20,
        prog: None,
    })
    .unwrap();
    let directory = directory();

    fs::create_dir_all(&directory).unwrap();

    for out in [OutType::Internal, OutType::Out] {
        let prog = generate_for(&RUN_PROG_TEMPLATE, PROG_CLASS, &prog_payload, out).unwrap();
        let snippet = generate_snippet(SNIPPET_CLASS, &snippet_payload, SNIPPET, out)
            .unwrap()
            .source;

        for (class, source) in [(PROG_CLASS, prog), (SNIPPET_CLASS, snippet)] {
            let path = directory.join(format!("{class}-{}.abap", out.interface()));

            fs::write(&path, source).unwrap();
            println!("{}", path.display());
        }
    }
}

//! The stability surface of `abapreach::extension`: one `use` that names every item an extension
//! may build on. A removed or renamed item breaks this file, so the list cannot shrink silently.
//! Adding an item is allowed; removing or retyping one is an incompatible change of the extension
//! point.

#![allow(unused_imports)]

use abapreach::extension::{
    Backend, BackendKind, CancelFlag, Cell, CellKind, ColumnRead, Connection, CreateRequest,
    DEFAULT_PACKAGE, DeleteBackend, DeleteKind, DeleteReply, DeleteStatus, Direction, DoctorProbe,
    Exit, Extension, ExtensionDelete, ExtensionKind, ExtensionOp, ExtensionOptions,
    FUNCTION_NOT_FOUND_KEY, Factories, FunctionApi, FunctionCall, FunctionInfo, FunctionResult,
    INTERRUPT_EXIT_CODE, InactiveFact, KindSpec, LineRange, MAX_CALL_CHARS, MAX_CALL_PARAMETERS,
    MAX_CALL_ROWS, MixedDelete, Normalize, ObjectFacts, OptionSpec, Outcome, ParameterInfo,
    ParameterType, Plan, ReadError, ReadRequest, Reconcile, Registry, RfcProbe, RunContext,
    SharedFactories, SourceFact, Status, TableRead, TableWrite, TrLock, TransportError, Verbs,
    adt_label, adt_pin, emit_lines, run_main, run_with, source_hash,
};
// Fault injection, in builds that have it.
#[cfg(feature = "fault-injection")]
use abapreach::extension::{ENV, Fault, Target, delete_reply_dropped, note_delete_reply_dropped};

#[test]
fn the_surface_resolves() {
    // The imports above are the test; a few values prove they are the items and not stand-ins.
    assert_eq!(INTERRUPT_EXIT_CODE, 130);
    assert_eq!(DEFAULT_PACKAGE, "$TMP");
    assert_eq!(FUNCTION_NOT_FOUND_KEY, "FU_NOT_FOUND");
    assert_eq!(adt_label(BackendKind::AdtHttp), "adt");
    assert_eq!(adt_label(BackendKind::RfcSadt), "rfc (SADT)");
    assert!(Verbs::READ.or(Verbs::DELETE).contains(Verbs::DELETE));
    assert!(!Verbs::READ.contains(Verbs::CREATE));
    assert_eq!(Registry::new().version_text(), env!("CARGO_PKG_VERSION"));
    assert_eq!(MAX_CALL_CHARS, 255);
}

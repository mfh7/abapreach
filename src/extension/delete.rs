//! The delete batch that mixes backends: the rows of an extension type run on the extension's own
//! backend, every other row on the ADT or SADT backend the router picked.

use crate::backend::BackendKind;
use crate::cli::DeleteKind;
use crate::operation::delete::{DeleteBackend, DeleteReply, ObjectFacts, Plan, Reconcile};

/// The delete backend of an extension's rows.
pub trait ExtensionDelete: DeleteBackend {
    /// Whether a `DELETE` request was made: the router may not fall back to another backend
    /// afterwards, because the report of that request would be lost.
    fn wrote(&self) -> bool;
}

/// The label of the ADT side of a batch, by the backend the router picked.
pub fn adt_label(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::AdtHttp => "adt",
        BackendKind::RfcSadt => "rfc (SADT)",
    }
}

/// The ADT backend a plan was simulated on: the label of its first object that an ADT backend
/// handled. `None` when the plan records none (only extension rows, or a plan of another tool).
/// An extension label never equals an ADT label (`Registry::with` refuses it).
pub fn adt_pin(plan: &Plan) -> Option<BackendKind> {
    plan.backends.iter().find_map(|(_, _, label)| {
        [BackendKind::AdtHttp, BackendKind::RfcSadt]
            .into_iter()
            .find(|backend| adt_label(*backend) == label)
    })
}

/// Each row goes to the backend that owns its kind, for the simulate and the delete alike.
pub struct MixedDelete<'a> {
    adt: &'a mut dyn DeleteBackend,
    adt_label: &'static str,
    extension: &'a mut dyn ExtensionDelete,
}

impl<'a> MixedDelete<'a> {
    pub fn new(
        adt: &'a mut dyn DeleteBackend,
        adt_label: &'static str,
        extension: &'a mut dyn ExtensionDelete,
    ) -> Self {
        Self {
            adt,
            adt_label,
            extension,
        }
    }

    fn owner(&mut self, kind: DeleteKind) -> &mut dyn DeleteBackend {
        match kind {
            DeleteKind::Extension(_) => &mut *self.extension,
            _ => &mut *self.adt,
        }
    }
}

impl DeleteBackend for MixedDelete<'_> {
    fn inspect(&mut self, kind: DeleteKind, name: &str) -> Result<Option<ObjectFacts>, String> {
        self.owner(kind).inspect(kind, name)
    }

    fn delete(&mut self, kind: DeleteKind, name: &str, tr: Option<&str>) -> DeleteReply {
        self.owner(kind).delete(kind, name, tr)
    }

    fn reconcile(&mut self, kind: DeleteKind, name: &str) -> Reconcile {
        self.owner(kind).reconcile(kind, name)
    }

    fn is_include(&mut self, kind: DeleteKind, name: &str) -> bool {
        self.owner(kind).is_include(kind, name)
    }

    /// The extension gets the complete batch list first; an `Err` of either side is a STOP before
    /// the first object.
    fn begin(&mut self, rows: &[(DeleteKind, String)], tr: Option<&str>) -> Result<(), String> {
        self.extension.begin(rows, tr)?;
        self.adt.begin(rows, tr)
    }

    fn begin_failure(&self, message: &str) -> (i32, String) {
        self.extension.begin_failure(message)
    }

    fn halted(&self) -> bool {
        self.adt.halted() || self.extension.halted()
    }

    fn backend_label(&self, kind: DeleteKind) -> Option<String> {
        match kind {
            DeleteKind::Extension(extension) => Some(extension.spec().backend_label.to_owned()),
            _ => Some(self.adt_label.to_owned()),
        }
    }
}

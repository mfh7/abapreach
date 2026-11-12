//! The built-in DDIC and `TRAN` types in the delete order, and the extension registry that must
//! not collide with them (M6 Task 17).

use abapreach::cli::{DeleteKind, DeleteTarget};
use abapreach::extension::{
    Backend, Connection, CreateRequest, Extension, ExtensionDelete, ExtensionKind, ExtensionOp,
    KindSpec, Normalize, OptionSpec, Outcome, ReadRequest, Registry, RunContext, Verbs,
};
use abapreach::operation::payload::Kind;

#[test]
fn the_new_types_take_rank_zero_and_six_to_nine_and_the_old_ranks_stay() {
    let rank = |token: &str| {
        DeleteTarget::parse_cli(&format!("{token}:ZX"), &[])
            .unwrap()
            .kind
            .rank()
    };

    // Ranks 1-5 belong to the standard types and to the private extension that sorts around them.
    assert_eq!(
        ["FUNC", "CLAS", "INTF", "PROG", "FUGR"].map(rank),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(
        ["TRAN", "TTYP", "TABL", "DTEL", "DOMA"].map(rank),
        [0, 6, 7, 8, 9]
    );
    assert_eq!(DeleteKind::Runner(Kind::Tran).rank(), 0);
    assert_eq!(DeleteKind::Runner(Kind::Doma).rank(), 9);
}

#[test]
fn the_new_types_have_their_own_name_limits_and_tokens() {
    for (kind, token, max) in [
        (Kind::Dtel, "DTEL", 30),
        (Kind::Doma, "DOMA", 30),
        (Kind::Tabl, "TABL", 30),
        (Kind::Ttyp, "TTYP", 30),
        (Kind::Tran, "TRAN", 20),
    ] {
        assert_eq!(DeleteKind::Runner(kind).token(), token);
        assert_eq!(DeleteKind::Runner(kind).max_name(), max, "{token}");
        assert_eq!(kind.token(), token);
        assert_eq!(kind.max_name(), max, "{token}");
    }
}

#[test]
fn the_delete_parser_of_the_public_grammar_knows_the_new_types_and_the_extension_parser_does_not() {
    assert_eq!(
        DeleteTarget::parse_cli("TRAN:ZT", &[]).unwrap().kind,
        DeleteKind::Runner(Kind::Tran)
    );
    // The MCP grammar is Task 26's: its parser keeps the old list.
    assert!(DeleteTarget::parse("TRAN:ZT", &[]).is_err());
    assert!(DeleteTarget::parse("DTEL:ZT", &[]).is_err());
}

/// An extension whose only variable part is one kind list.
struct Colliding {
    kinds: &'static [ExtensionKind],
}

impl Extension for Colliding {
    fn name(&self) -> &'static str {
        "colliding"
    }

    fn kinds(&self) -> &'static [ExtensionKind] {
        self.kinds
    }

    fn preflight(
        &self,
        _: ExtensionOp,
        _: ExtensionKind,
        _: &Connection,
        _: Option<Backend>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn read(&self, _: ExtensionKind, _: &ReadRequest<'_>, _: RunContext<'_>) -> Outcome {
        unreachable!()
    }

    fn create(&self, _: ExtensionKind, _: &CreateRequest<'_>, _: RunContext<'_>) -> Outcome {
        unreachable!()
    }

    fn delete_backend<'r>(
        &'r self,
        _: RunContext<'r>,
    ) -> Result<Box<dyn ExtensionDelete + 'r>, Outcome> {
        unreachable!()
    }
}

const fn spec(token: &'static str, options: &'static [OptionSpec]) -> KindSpec {
    KindSpec {
        token,
        help: "",
        verbs: Verbs::READ,
        deploy_refusal: None,
        max_name: 20,
        max_text: 36,
        create_options: options,
        delete_rank: 0,
        backend_label: "y (YKIND)",
        plan_backend_optional: false,
        is_self: None,
        mcp_read_refusal: None,
    }
}

const fn option(id: &'static str, long: &'static str) -> OptionSpec {
    OptionSpec {
        id,
        long,
        value_name: "V",
        help: "",
        mcp_description: "",
        normalize: Normalize::Verbatim,
        misuse: "",
    }
}

static TRAN: KindSpec = spec("TRAN", &[]);
static DTEL: KindSpec = spec("DTEL", &[]);
static DOMA: KindSpec = spec("DOMA", &[]);
static TABL: KindSpec = spec("TABL", &[]);
static TTYP: KindSpec = spec("TTYP", &[]);
static PROG_OPTION: [OptionSpec; 1] = [option("prog", "prog")];
static PROG_LONG: [OptionSpec; 1] = [option("report", "prog")];
static WITH_PROG: KindSpec = spec("YKIND", &PROG_OPTION);
static WITH_PROG_LONG: KindSpec = spec("YKIND", &PROG_LONG);
static TRAN_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&TRAN)];
static DTEL_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&DTEL)];
static DOMA_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&DOMA)];
static TABL_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&TABL)];
static TTYP_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&TTYP)];
static PROG_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&WITH_PROG)];
static PROG_LONG_KINDS: [ExtensionKind; 1] = [ExtensionKind::new(&WITH_PROG_LONG)];

#[test]
#[should_panic(expected = "extension type TRAN is a standard type")]
fn an_extension_that_defines_tran_is_refused_with_the_duplicate_token_message() {
    let _ = Registry::new().with(Colliding { kinds: &TRAN_KINDS });
}

#[test]
#[should_panic(expected = "extension type DTEL is a standard type")]
fn an_extension_that_defines_dtel_is_refused() {
    let _ = Registry::new().with(Colliding { kinds: &DTEL_KINDS });
}

#[test]
#[should_panic(expected = "extension type DOMA is a standard type")]
fn an_extension_that_defines_doma_is_refused() {
    let _ = Registry::new().with(Colliding { kinds: &DOMA_KINDS });
}

#[test]
#[should_panic(expected = "extension type TABL is a standard type")]
fn an_extension_that_defines_tabl_is_refused() {
    let _ = Registry::new().with(Colliding { kinds: &TABL_KINDS });
}

#[test]
#[should_panic(expected = "extension type TTYP is a standard type")]
fn an_extension_that_defines_ttyp_is_refused() {
    let _ = Registry::new().with(Colliding { kinds: &TTYP_KINDS });
}

#[test]
#[should_panic(expected = "extension option prog is a standard create argument")]
fn an_extension_option_with_the_id_prog_is_refused() {
    let _ = Registry::new().with(Colliding { kinds: &PROG_KINDS });
}

#[test]
#[should_panic(expected = "extension option prog is a standard create argument")]
fn an_extension_option_with_the_long_flag_prog_is_refused() {
    let _ = Registry::new().with(Colliding {
        kinds: &PROG_LONG_KINDS,
    });
}

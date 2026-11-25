mod common;

use abapreach::operation::dictionary::{
    BASE_ROWS, BaseRow, CheckError, ClassRow, Dictionary, GeneratedViewRow, MAX_LOOKUPS,
    TransportDictionary, check_entities,
};
use abapreach::operation::read_table::ReadTableError;
use abapreach::operation::sql::Select;
use abapreach::policy::Policies;
use abapreach::transport::{Response, TransportError};
use common::{ScriptedTransport, fixture_path_in, preview};
use std::collections::{HashMap, HashSet};
use std::fs;

/// An in-memory dictionary that records every lookup.
#[derive(Default)]
struct Fake {
    classes: HashMap<String, Vec<ClassRow>>,
    generated: HashSet<String>,
    bases: HashMap<String, Vec<BaseRow>>,
    broken: HashSet<String>,
    lookups: Vec<String>,
}

fn class_row(name: &str, class: &str, view_class: Option<&str>) -> ClassRow {
    ClassRow {
        name: name.to_owned(),
        table_class: class.to_owned(),
        view_class: view_class.map(str::to_owned),
    }
}

impl Fake {
    fn table(mut self, name: &str) -> Self {
        self.classes
            .insert(name.to_owned(), vec![class_row(name, "TRANSP", None)]);
        self
    }

    fn class(mut self, name: &str, class: &str) -> Self {
        self.classes
            .insert(name.to_owned(), vec![class_row(name, class, None)]);
        self
    }

    fn view(mut self, name: &str, bases: &[&str]) -> Self {
        self.classes
            .insert(name.to_owned(), vec![class_row(name, "VIEW", Some("D"))]);
        self.bases.insert(
            name.to_owned(),
            bases
                .iter()
                .map(|table| BaseRow {
                    view: name.to_owned(),
                    table: (*table).to_owned(),
                })
                .collect(),
        );
        self
    }

    fn generated(mut self, name: &str) -> Self {
        self.generated.insert(name.to_owned());
        self
    }
}

impl Dictionary for Fake {
    type Error = String;

    fn class_rows(&mut self, name: &str) -> Result<Vec<ClassRow>, String> {
        self.lookups.push(format!("DD02L {name}"));

        if self.broken.contains(name) {
            return Err(format!("lookup of {name} failed"));
        }

        Ok(self.classes.get(name).cloned().unwrap_or_default())
    }

    fn generated_view_rows(&mut self, name: &str) -> Result<Vec<GeneratedViewRow>, String> {
        self.lookups.push(format!("DDLDEPENDENCY {name}"));

        Ok(if self.generated.contains(name) {
            vec![GeneratedViewRow {
                object_name: name.to_owned(),
                object_type: "VIEW".to_owned(),
            }]
        } else {
            Vec::new()
        })
    }

    fn base_rows(&mut self, view: &str) -> Result<Vec<BaseRow>, String> {
        self.lookups.push(format!("DD26S {view}"));

        Ok(self.bases.get(view).cloned().unwrap_or_default())
    }
}

fn names(entities: &[&str]) -> Vec<String> {
    entities.iter().map(|name| (*name).to_owned()).collect()
}

fn check(fake: &mut Fake, entities: &[&str]) -> Result<(), CheckError<String>> {
    check_entities(fake, &names(entities), &Policies::default())
}

fn refusal(result: Result<(), CheckError<String>>) -> String {
    match result {
        Err(CheckError::Refused(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn transparent_cluster_and_pool_tables_pass_after_one_lookup() {
    for class in ["TRANSP", "CLUSTER", "POOL"] {
        let mut fake = Fake::default().class("TAB", class);

        assert_eq!(check(&mut fake, &["TAB"]), Ok(()), "{class}");
        assert_eq!(fake.lookups, ["DD02L TAB"]);
    }
}

#[test]
fn an_unknown_name_is_a_stop_and_a_cds_entity_name_is_unknown() {
    // The DDL name of a CDS entity is no DD02L entry, only its generated SQL view is.
    let mut fake = Fake::default().table("T001");
    let message = refusal(check(&mut fake, &["I_COMPANYCODE"]));

    assert!(
        message.contains("I_COMPANYCODE is not in the ABAP dictionary as an active table or view"),
        "{message}"
    );
    assert_eq!(fake.lookups, ["DD02L I_COMPANYCODE"]);
}

#[test]
fn a_generated_cds_sql_view_is_a_stop_before_its_base_tables_are_read() {
    let mut fake = Fake::default()
        .view("ICOMPANYCODE", &["T001"])
        .table("T001")
        .generated("ICOMPANYCODE");
    let message = refusal(check(&mut fake, &["ICOMPANYCODE"]));

    assert!(message.contains("generated CDS SQL view"), "{message}");
    assert_eq!(
        fake.lookups,
        ["DD02L ICOMPANYCODE", "DDLDEPENDENCY ICOMPANYCODE"]
    );
}

#[test]
fn any_dependency_row_refuses_the_view() {
    let rows = |object_name: &str, object_type: &str| {
        vec![GeneratedViewRow {
            object_name: object_name.to_owned(),
            object_type: object_type.to_owned(),
        }]
    };

    struct Rows(Vec<GeneratedViewRow>);

    impl Dictionary for Rows {
        type Error = String;

        fn class_rows(&mut self, name: &str) -> Result<Vec<ClassRow>, String> {
            Ok(vec![class_row(
                name,
                if name == "V" { "VIEW" } else { "TRANSP" },
                Some("D"),
            )])
        }

        fn generated_view_rows(&mut self, _: &str) -> Result<Vec<GeneratedViewRow>, String> {
            Ok(self.0.clone())
        }

        fn base_rows(&mut self, view: &str) -> Result<Vec<BaseRow>, String> {
            Ok(vec![BaseRow {
                view: view.to_owned(),
                table: "T".to_owned(),
            }])
        }
    }

    let run = |rows: Vec<GeneratedViewRow>| {
        check_entities(&mut Rows(rows), &names(&["V"]), &Policies::default())
    };

    assert!(run(rows("V", "VIEW")).is_err());
    assert!(run(rows("V", "TABL")).is_err());
    // The lookup asks for this view only: any row refuses, whatever it names or its type.
    assert!(run(rows("OTHER", "VIEW")).is_err());
    assert_eq!(run(Vec::new()), Ok(()));
}

#[test]
fn a_view_without_dd26s_rows_is_a_stop() {
    let mut fake = Fake::default().view("ZV", &[]);
    let message = refusal(check(&mut fake, &["ZV"]));

    assert!(
        message.contains("the base tables of ZV are missing"),
        "{message}"
    );
}

#[test]
fn a_view_with_too_many_base_rows_is_a_stop() {
    let tables: Vec<String> = (0..BASE_ROWS).map(|index| format!("T{index}")).collect();
    let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
    let mut fake = Fake::default().view("ZV", &tables);

    assert!(refusal(check(&mut fake, &["ZV"])).contains("too many to check"));
}

#[test]
fn a_view_over_allowed_tables_passes_and_every_base_table_is_classified() {
    let mut fake = Fake::default()
        .view("ZV", &["T001", "T001W"])
        .table("T001")
        .class("T001W", "CLUSTER");

    assert_eq!(check(&mut fake, &["ZV"]), Ok(()));
    assert_eq!(
        fake.lookups,
        [
            "DD02L ZV",
            "DDLDEPENDENCY ZV",
            "DD26S ZV",
            "DD02L T001",
            "DD02L T001W"
        ]
    );
}

#[test]
fn a_denied_base_table_names_the_view_and_the_table() {
    let mut fake = Fake::default().view("ZV", &["T001", "usr02"]).table("T001");
    let message = refusal(check(&mut fake, &["ZV"]));

    assert_eq!(message, "view ZV reads table USR02 (read_table_denied)");
}

#[test]
fn the_deny_list_in_force_decides_for_base_tables() {
    let policies = Policies {
        read_table_denied: Vec::new(),
        ..Policies::default()
    };
    let mut fake = Fake::default().view("ZV", &["USR02"]).table("USR02");

    assert_eq!(
        check_entities(&mut fake, &names(&["ZV"]), &policies),
        Ok(())
    );

    let policies = Policies {
        read_table_denied: vec!["T001".to_owned()],
        ..Policies::default()
    };
    let mut fake = Fake::default().view("ZV", &["t001"]).table("T001");

    assert!(check_entities(&mut fake, &names(&["ZV"]), &policies).is_err());
}

#[test]
fn a_denied_entity_is_refused_before_any_lookup() {
    let mut fake = Fake::default().table("USR02");
    let message = refusal(check(&mut fake, &["USR02"]));

    assert_eq!(message, "table USR02 is on the read_table_denied list");
    assert!(fake.lookups.is_empty());
}

#[test]
fn a_base_that_is_unknown_or_no_table_is_a_stop() {
    let mut fake = Fake::default().view("ZV", &["GONE"]);

    assert!(refusal(check(&mut fake, &["ZV"])).contains("GONE is not in the ABAP dictionary"));

    for class in ["INTTAB", "APPEND", ""] {
        let mut fake = Fake::default().view("ZV", &["S"]).class("S", class);

        assert!(
            refusal(check(&mut fake, &["ZV"])).contains("neither a table nor a database view"),
            "{class:?}"
        );
    }
}

#[test]
fn a_view_base_is_checked_in_turn_up_to_two_levels() {
    let mut fake = Fake::default()
        .view("V1", &["V2"])
        .view("V2", &["T001"])
        .table("T001");

    assert_eq!(check(&mut fake, &["V1"]), Ok(()));

    let mut fake = Fake::default()
        .view("V1", &["V2"])
        .view("V2", &["V3"])
        .view("V3", &["T001"])
        .table("T001");

    assert!(refusal(check(&mut fake, &["V1"])).contains("V3 is nested too deeply"));
}

#[test]
fn a_view_that_reads_itself_is_a_stop() {
    let mut fake = Fake::default().view("V1", &["V2"]).view("V2", &["V1"]);

    assert!(refusal(check(&mut fake, &["V1"])).contains("reads itself"));
}

#[test]
fn a_generated_cds_view_below_a_view_is_a_stop() {
    let mut fake = Fake::default()
        .view("V1", &["V2"])
        .view("V2", &["T001"])
        .table("T001")
        .generated("V2");

    assert!(refusal(check(&mut fake, &["V1"])).contains("V2 is a generated CDS SQL view"));
}

#[test]
fn a_name_is_looked_up_once() {
    let mut fake = Fake::default()
        .view("V1", &["T001"])
        .view("V2", &["T001"])
        .table("T001");

    assert_eq!(check(&mut fake, &["V1", "V2", "T001", "V1"]), Ok(()));
    assert_eq!(
        fake.lookups
            .iter()
            .filter(|call| *call == "DD02L T001")
            .count(),
        1
    );
    assert_eq!(
        fake.lookups
            .iter()
            .filter(|call| *call == "DD02L V1")
            .count(),
        1
    );
}

#[test]
fn a_dictionary_row_that_is_not_one_row_for_the_name_is_a_stop() {
    let mut fake = Fake::default();

    fake.classes.insert(
        "T".to_owned(),
        vec![
            class_row("T", "TRANSP", None),
            class_row("T", "VIEW", Some("D")),
        ],
    );

    assert!(refusal(check(&mut fake, &["T"])).contains("more than one dictionary entry"));

    let mut fake = Fake::default();

    fake.classes
        .insert("T".to_owned(), vec![class_row("OTHER", "TRANSP", None)]);

    assert!(refusal(check(&mut fake, &["T"])).contains("did not return T"));
}

#[test]
fn a_view_that_is_no_database_view_is_a_stop() {
    for view_class in [Some("V"), Some("P"), Some("H"), Some("C"), Some(""), None] {
        let mut fake = Fake::default();

        fake.classes
            .insert("T".to_owned(), vec![class_row("T", "VIEW", view_class)]);

        assert!(
            refusal(check(&mut fake, &["T"])).contains("is not a database view"),
            "{view_class:?}"
        );
        assert_eq!(fake.lookups, ["DD02L T"], "{view_class:?}");
    }
}

#[test]
fn base_rows_of_another_view_or_without_a_table_are_unreadable() {
    for (view, table) in [("OTHER", "T001"), ("ZV", "")] {
        let mut fake = Fake::default().view("ZV", &[]).table("T001");

        fake.bases.insert(
            "ZV".to_owned(),
            vec![BaseRow {
                view: view.to_owned(),
                table: table.to_owned(),
            }],
        );

        assert!(
            refusal(check(&mut fake, &["ZV"])).contains("the base tables of ZV are unreadable")
        );
    }
}

#[test]
fn a_failed_lookup_is_returned_as_it_is() {
    let mut fake = Fake::default().table("T");

    fake.broken.insert("T".to_owned());

    assert_eq!(
        check(&mut fake, &["T"]),
        Err(CheckError::Lookup("lookup of T failed".to_owned()))
    );
}

#[test]
fn the_lookup_budget_stops_a_statement_that_needs_too_many() {
    let tables: Vec<String> = (0..=MAX_LOOKUPS).map(|index| format!("T{index}")).collect();
    let mut fake = Fake::default();

    for table in &tables {
        fake = fake.table(table);
    }

    let all: Vec<&str> = tables.iter().map(String::as_str).collect();

    assert_eq!(check(&mut fake, &all[..MAX_LOOKUPS]), Ok(()));
    assert_eq!(fake.lookups.len(), MAX_LOOKUPS);

    let mut fake = Fake::default();

    for table in &tables {
        fake = fake.table(table);
    }

    let message = refusal(check(&mut fake, &all));

    assert!(message.contains("more than 24 lookups"), "{message}");
    assert_eq!(fake.lookups.len(), MAX_LOOKUPS);
}

#[test]
fn a_statement_is_checked_through_every_from_and_join_entity() {
    let select = Select::parse(
        "SELECT a~x FROM t001 AS a JOIN zv AS b ON a~k = b~k JOIN t001w AS c ON a~k = c~k",
    )
    .unwrap();
    let mut fake = Fake::default()
        .table("T001")
        .table("T001W")
        .view("ZV", &["USR02"]);
    let message = refusal(check_entities(
        &mut fake,
        &select.entities(),
        &Policies::default(),
    ));

    assert_eq!(message, "view ZV reads table USR02 (read_table_denied)");

    let mut fake = Fake::default()
        .table("T001")
        .table("T001W")
        .view("ZV", &["T000"])
        .table("T000");

    assert_eq!(
        check_entities(&mut fake, &select.entities(), &Policies::default()),
        Ok(())
    );
}

// The data preview implementation, on the live reply fixtures of the three releases.

fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_path_in("m9", name))
        .unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn reply(xml: String) -> Result<Response, TransportError> {
    Ok(Response::text(200, xml))
}

fn dd02l(name: &str, class: &str, view_class: &str) -> Result<Response, TransportError> {
    reply(preview(&[
        ("TABNAME", &[Some(name)]),
        ("TABCLASS", &[Some(class)]),
        ("VIEWCLASS", &[Some(view_class)]),
    ]))
}

fn no_dependency() -> Result<Response, TransportError> {
    reply(preview(&[
        ("DDLNAME", &[]),
        ("OBJECTNAME", &[]),
        ("OBJECTTYPE", &[]),
    ]))
}

#[test]
fn the_dd02l_fixtures_give_the_table_classes() {
    for release in ["740", "750", "758"] {
        let mut transport = ScriptedTransport::replies(vec![reply(fixture(&format!(
            "dd02l-entity-{release}.xml"
        )))]);
        let rows = TransportDictionary::new(&mut transport)
            .class_rows("TADIR")
            .unwrap();
        let mut classes: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.table_class.as_str()))
            .collect();

        classes.sort_unstable();

        assert_eq!(
            classes,
            [
                ("TADIR", "TRANSP"),
                ("USR02", "TRANSP"),
                ("V_USR_NAME", "VIEW")
            ],
            "{release}"
        );
        assert!(rows.iter().all(|row| row.view_class.is_none()), "{release}");
    }
}

#[test]
fn the_dd26s_fixtures_give_the_base_tables() {
    for release in ["740", "750", "758"] {
        let mut transport =
            ScriptedTransport::replies(vec![reply(fixture(&format!("dd26s-view-{release}.xml")))]);
        let rows = TransportDictionary::new(&mut transport)
            .base_rows("V_USR_NAME")
            .unwrap();

        assert_eq!(
            rows,
            [
                BaseRow {
                    view: "V_USR_NAME".to_owned(),
                    table: "ADRP".to_owned()
                },
                BaseRow {
                    view: "V_USR_NAME".to_owned(),
                    table: "USR21".to_owned()
                }
            ],
            "{release}"
        );
    }
}

#[test]
fn a_view_over_the_dd26s_fixture_tables_passes_and_a_denied_base_stops() {
    for release in ["740", "750", "758"] {
        let script = || {
            vec![
                dd02l("V_USR_NAME", "VIEW", "D"),
                no_dependency(),
                reply(fixture(&format!("dd26s-view-{release}.xml"))),
                dd02l("ADRP", "TRANSP", ""),
                dd02l("USR21", "TRANSP", ""),
            ]
        };
        let mut transport = ScriptedTransport::replies(script());

        assert!(
            check_entities(
                &mut TransportDictionary::new(&mut transport),
                &names(&["V_USR_NAME"]),
                &Policies::default(),
            )
            .is_ok(),
            "{release}"
        );
        transport.assert_finished();

        let policies = Policies {
            read_table_denied: vec!["USR21".to_owned()],
            ..Policies::default()
        };
        let mut replies = script();

        replies.truncate(4);

        let mut transport = ScriptedTransport::replies(replies);
        let result = check_entities(
            &mut TransportDictionary::new(&mut transport),
            &names(&["V_USR_NAME"]),
            &policies,
        );

        assert_eq!(
            result,
            Err(CheckError::Refused(
                "view V_USR_NAME reads table USR21 (read_table_denied)".to_owned()
            )),
            "{release}"
        );
    }
}

#[test]
fn the_ddldependency_fixtures_mark_a_generated_cds_sql_view() {
    // Each fixture lists real generated views: the lookup of one of them must stop the check.
    for (release, generated) in [
        ("740", "SEPM_ILEAVEREQ"),
        ("750", "IBASISID"),
        ("750", "ISDSCHEDLINECAT"),
        ("758", "ISRCHTRM"),
    ] {
        let mut transport = ScriptedTransport::replies(vec![
            dd02l(generated, "VIEW", "D"),
            reply(fixture(&format!("ddldependency-{release}.xml"))),
        ]);
        let result = check_entities(
            &mut TransportDictionary::new(&mut transport),
            &names(&[generated]),
            &Policies::default(),
        );

        match result {
            Err(CheckError::Refused(message)) => {
                assert!(
                    message.contains("generated CDS SQL view"),
                    "{release} {message}"
                );
            }
            other => panic!("{release} {generated}: {other:?}"),
        }
    }
}

#[test]
fn the_dependency_lookup_names_the_view_and_the_object_type() {
    let mut transport = ScriptedTransport::replies(vec![reply(fixture("ddldependency-758.xml"))]);
    let rows = TransportDictionary::new(&mut transport)
        .generated_view_rows("ISRCHTRM")
        .unwrap();

    assert!(rows.iter().all(|row| row.object_type == "VIEW"));
    assert!(rows.iter().any(|row| row.object_name == "ISRCHTRM"));

    let call = &transport.calls[0];

    assert_eq!(
        String::from_utf8(call.body.clone()).unwrap(),
        "SELECT DDLNAME, OBJECTNAME, OBJECTTYPE FROM DDLDEPENDENCY WHERE OBJECTNAME = 'ISRCHTRM' AND OBJECTTYPE = 'VIEW'"
    );
    assert!(call.uri.ends_with("ddicEntityName=DDLDEPENDENCY"));
}

#[test]
fn the_dependency_check_fails_closed_when_the_table_cannot_be_read() {
    for answer in [
        Ok(Response::text(500, "boom")),
        Ok(Response::text(200, "not xml <<")),
        reply(preview(&[("OTHER", &[Some("x")])])),
    ] {
        let mut transport = ScriptedTransport::replies(vec![dd02l("V", "VIEW", "D"), answer]);
        let result = check_entities(
            &mut TransportDictionary::new(&mut transport),
            &names(&["V"]),
            &Policies::default(),
        );

        assert!(
            matches!(
                &result,
                Err(CheckError::Lookup(ReadTableError::Rejected(_)))
            ),
            "{result:?}"
        );
        assert_eq!(transport.calls.len(), 2);
    }
}

#[test]
fn a_lost_connection_during_the_check_keeps_its_routing_class() {
    let mut transport = ScriptedTransport::replies(vec![Err(TransportError::PreSendConnectivity(
        "connection refused".to_owned(),
    ))]);
    let result = check_entities(
        &mut TransportDictionary::new(&mut transport),
        &names(&["T001"]),
        &Policies::default(),
    );

    assert!(
        matches!(
            result,
            Err(CheckError::Lookup(ReadTableError::Failed { .. }))
        ),
        "{result:?}"
    );
}

fn sql_view_script(
    class: &str,
    dependency: &str,
    bases: &str,
) -> Vec<Result<Response, TransportError>> {
    vec![
        reply(fixture(class)),
        reply(fixture(dependency)),
        reply(fixture(bases)),
        dd02l("T005", "TRANSP", ""),
    ]
}

#[test]
fn a_cds_ddl_name_is_refused_as_an_unknown_name_on_the_live_758_reply() {
    let mut transport =
        ScriptedTransport::replies(vec![reply(fixture("dd02l-cds-ddl-name-758.xml"))]);
    let result = check_entities(
        &mut TransportDictionary::new(&mut transport),
        &names(&["I_COUNTRY"]),
        &Policies::default(),
    );

    match result {
        Err(CheckError::Refused(message)) => assert!(
            message.contains("I_COUNTRY is not in the ABAP dictionary as an active table or view"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }

    assert_eq!(transport.calls.len(), 1);
}

#[test]
fn the_ddldependency_gate_refuses_the_generated_view_that_dd02l_and_dd26s_alone_pass() {
    // Without the DDLDEPENDENCY answer the live DD02L and DD26S replies vouch for the view.
    let mut transport = ScriptedTransport::replies(vec![
        reply(fixture("dd02l-cds-sql-view-758.xml")),
        no_dependency(),
        reply(fixture("dd26s-cds-sql-view-758.xml")),
        dd02l("T005", "TRANSP", ""),
    ]);

    assert_eq!(
        check_entities(
            &mut TransportDictionary::new(&mut transport),
            &names(&["IFICOUNTRY"]),
            &Policies::default(),
        ),
        Ok(())
    );
    transport.assert_finished();

    // With the live DDLDEPENDENCY row the view is refused before DD26S is read.
    let mut replies = sql_view_script(
        "dd02l-cds-sql-view-758.xml",
        "ddldependency-cds-sql-view-758.xml",
        "dd26s-cds-sql-view-758.xml",
    );

    replies.truncate(2);

    let mut transport = ScriptedTransport::replies(replies);
    let result = check_entities(
        &mut TransportDictionary::new(&mut transport),
        &names(&["IFICOUNTRY"]),
        &Policies::default(),
    );

    match result {
        Err(CheckError::Refused(message)) => {
            assert!(
                message.contains("IFICOUNTRY is a generated CDS SQL view"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }

    assert_eq!(transport.calls.len(), 2);
    transport.assert_finished();
}

#[test]
fn a_check_error_prints_its_message_and_is_an_error() {
    let refused: CheckError<String> = CheckError::Refused("T is refused".to_owned());
    let lookup: CheckError<String> = CheckError::Lookup("no answer".to_owned());

    assert_eq!(refused.to_string(), "T is refused");
    assert_eq!(lookup.to_string(), "no answer");

    let boxed: Box<dyn std::error::Error> = Box::new(refused);

    assert_eq!(boxed.to_string(), "T is refused");
}

#[test]
fn a_base_table_cell_that_is_no_identifier_is_a_stop_and_never_reaches_a_body() {
    let long = "T".repeat(31);

    for cell in [
        "T001' OR 'A' = 'A",
        "T 001",
        "T-1",
        "T001'",
        "/NS/",
        "//T",
        long.as_str(),
    ] {
        let mut fake = Fake::default().view("ZV", &[cell]).table("T001");
        let message = refusal(check(&mut fake, &["ZV"]));

        assert!(
            message.contains("no DDIC identifier"),
            "{cell:?}: {message}"
        );
        assert_eq!(
            fake.lookups,
            ["DD02L ZV", "DDLDEPENDENCY ZV", "DD26S ZV"],
            "{cell:?}"
        );
    }

    let mut fake = Fake::default()
        .view("ZV", &["/NS/TAB", "t001"])
        .table("/NS/TAB")
        .table("T001");

    assert_eq!(check(&mut fake, &["ZV"]), Ok(()));

    // On the data preview: the quote-bearing cell stops the check with no further request.
    let mut transport = ScriptedTransport::replies(vec![
        dd02l("ZV", "VIEW", "D"),
        no_dependency(),
        reply(preview(&[
            ("VIEWNAME", &[Some("ZV")]),
            ("TABNAME", &[Some("T001' OR 'A' = 'A")]),
        ])),
    ]);
    let result = check_entities(
        &mut TransportDictionary::new(&mut transport),
        &names(&["ZV"]),
        &Policies::default(),
    );

    assert!(matches!(result, Err(CheckError::Refused(_))), "{result:?}");
    assert_eq!(transport.calls.len(), 3);
    transport.assert_finished();
}

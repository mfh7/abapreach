mod common;
mod replay;

use abapreach::backend::{BackendKind, Selection, SelectionReason};
use abapreach::config::Connection;
use abapreach::domain::{Outcome, Status};
use abapreach::operation::deploy::{CancelFlag, DeployInput, DeployProg};
use abapreach::policy::{Confirmer, ForeignInactive, Policies};
use abapreach::transport::{Request, Response};
use replay::{
    FOREIGN_INACTIVE_FEED, NO_INACTIVE_FEED, OWN_INACTIVE_FEED, Step, feed, fx, replay,
    versions_uri,
};
use std::cell::RefCell;

const FIXTURE: &str = "dev-750-rfc.json";
const USER: &str = "DEVUSER";
const OTHER: &str = "OTHERUSER";

/// A version list as SAP pretty-prints it: the values carry the indentation of their elements.
const PRETTY_FOREIGN_INACTIVE_FEED: &str = concat!(
    "<atom:feed xmlns:atom=\"http://www.w3.org/2005/Atom\">\n",
    "  <atom:entry>\n",
    "    <atom:author>\n",
    "      <atom:name>\n        OTHERUSER\n      </atom:name>\n",
    "    </atom:author>\n",
    "    <atom:id>\n      99999\n    </atom:id>\n",
    "  </atom:entry>\n",
    "  <atom:entry>\n",
    "    <atom:author>\n",
    "      <atom:name>\n        DEVUSER\n      </atom:name>\n",
    "    </atom:author>\n",
    "    <atom:id>\n      00001\n    </atom:id>\n",
    "  </atom:entry>\n",
    "</atom:feed>",
);

/// A version list whose values are wrapped in CDATA sections, as some gateways return them.
const CDATA_FOREIGN_INACTIVE_FEED: &str = concat!(
    "<atom:feed xmlns:atom=\"http://www.w3.org/2005/Atom\">",
    "<atom:entry><atom:author><atom:name><![CDATA[OTHERUSER]]></atom:name></atom:author>",
    "<atom:id><![CDATA[99999]]></atom:id></atom:entry>",
    "<atom:entry><atom:author><atom:name>DEVUSER</atom:name></atom:author>",
    "<atom:id>00001</atom:id></atom:entry></atom:feed>",
);

fn input(simulate: bool) -> DeployInput {
    DeployInput {
        name: "YTEST_ABAPREACH01".to_owned(),
        source: vec![
            "REPORT ytest_abapreach01.".to_owned(),
            String::new(),
            "WRITE / 'v2'.".to_owned(),
        ],
        base: None,
        simulate,
        tr: None,
    }
}

fn connection(policy: ForeignInactive) -> Connection {
    let mut connection = Connection::test_insecure();

    connection.user = USER.to_owned();
    connection.policies = Policies {
        foreign_inactive: policy,
    };
    // Verified TLS keeps the output free of the insecure-connection warning.
    if let Some(adt) = connection.adt.as_mut() {
        adt.insecure = false;
    }

    connection
}

/// The terminal seam of the `ask` policy: no terminal at all, or one that shows the prompt and
/// answers with a scripted yes or no.
#[derive(Default)]
struct FakeTerminal {
    interactive: bool,
    answer: bool,
    prompts: RefCell<Vec<String>>,
}

impl FakeTerminal {
    fn answering(answer: bool) -> Self {
        Self {
            interactive: true,
            answer,
            prompts: RefCell::new(Vec::new()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.borrow().clone()
    }
}

impl Confirmer for FakeTerminal {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn confirm(&self, prompt: &str) -> bool {
        self.prompts.borrow_mut().push(prompt.to_owned());

        self.answer
    }
}

/// A simulate that passes the guard: live read, version list, lock, locked re-read, version list
/// again, check, unlock.
fn simulate_steps(versions: &str) -> Vec<Step> {
    vec![
        fx(6),
        feed(versions),
        fx(7),
        fx(6),
        feed(versions),
        fx(8),
        fx(9),
    ]
}

fn locks(calls: &[Request]) -> usize {
    calls
        .iter()
        .filter(|call| call.uri.contains("_action=LOCK"))
        .count()
}

fn unlocks(calls: &[Request]) -> usize {
    calls
        .iter()
        .filter(|call| call.uri.contains("_action=UNLOCK"))
        .count()
}

fn puts(calls: &[Request]) -> usize {
    calls.iter().filter(|call| call.method == "PUT").count()
}

/// The verdict line of a run: the last line of stdout.
fn verdict(outcome: &Outcome) -> &str {
    outcome.stdout.lines().last().unwrap_or_default()
}

#[test]
fn a_foreign_inactive_version_is_rejected_before_the_lock_in_simulate_and_deploy() {
    for simulate in [true, false] {
        let mut transport = replay(FIXTURE, vec![fx(6), feed(FOREIGN_INACTIVE_FEED)]);
        let outcome = DeployProg::run(
            &mut transport,
            input(simulate),
            Some(&connection(ForeignInactive::Reject)),
        );

        assert_eq!(outcome.status, Some(Status::Rejected), "{simulate}");
        assert_eq!(outcome.exit, 1, "{simulate}");
        assert_eq!(
            verdict(&outcome),
            format!("REJECTED: inactive version of another user exists: {OTHER}"),
            "{simulate}"
        );
        assert_eq!(outcome.stderr, "", "{simulate}");
        assert_eq!(locks(&transport.calls), 0, "{simulate}");
        assert_eq!(transport.calls.len(), 2, "{simulate}");
        assert_eq!(transport.calls[1].uri, versions_uri(), "{simulate}");
        assert_eq!(transport.closes, 1);
        transport.assert_finished();
    }
}

#[test]
fn the_guard_reads_the_version_list_through_whitespace_and_cdata() {
    for body in [PRETTY_FOREIGN_INACTIVE_FEED, CDATA_FOREIGN_INACTIVE_FEED] {
        let mut transport = replay(FIXTURE, vec![fx(6), feed(body)]);
        let outcome = DeployProg::run(
            &mut transport,
            input(true),
            Some(&connection(ForeignInactive::Reject)),
        );

        assert_eq!(outcome.status, Some(Status::Rejected), "{body}");
        assert_eq!(
            verdict(&outcome),
            format!("REJECTED: inactive version of another user exists: {OTHER}"),
            "{body}"
        );
        assert_eq!(locks(&transport.calls), 0, "{body}");
        transport.assert_finished();
    }
}

#[test]
fn an_inactive_version_that_appears_before_the_lock_is_rejected_after_it() {
    for simulate in [true, false] {
        let mut transport = replay(
            FIXTURE,
            vec![
                fx(6),
                feed(NO_INACTIVE_FEED),
                fx(7),
                fx(6),
                feed(FOREIGN_INACTIVE_FEED),
                fx(9),
            ],
        );
        let outcome = DeployProg::run(
            &mut transport,
            input(simulate),
            Some(&connection(ForeignInactive::Reject)),
        );

        assert_eq!(outcome.status, Some(Status::Rejected), "{simulate}");
        assert_eq!(outcome.exit, 1, "{simulate}");
        assert_eq!(
            verdict(&outcome),
            format!("REJECTED: inactive version of another user exists: {OTHER}"),
            "{simulate}"
        );
        assert_eq!(locks(&transport.calls), 1, "{simulate}");
        assert_eq!(unlocks(&transport.calls), 1, "{simulate}");
        assert_eq!(puts(&transport.calls), 0, "{simulate}");
        assert_eq!(transport.closes, 1);
        transport.assert_finished();
    }
}

#[test]
fn an_unreadable_version_list_after_the_lock_is_rejected_and_unlocked() {
    let mut transport = replay(
        FIXTURE,
        vec![
            fx(6),
            feed(NO_INACTIVE_FEED),
            fx(7),
            fx(6),
            Step::Versions(Ok(Response::empty(500))),
            fx(9),
        ],
    );
    let outcome = DeployProg::run(
        &mut transport,
        input(false),
        Some(&connection(ForeignInactive::Reject)),
    );

    assert_eq!(outcome.status, Some(Status::Rejected));
    assert_eq!(
        verdict(&outcome),
        "REJECTED: version list unavailable: HTTP 500 (policy reject)"
    );
    assert_eq!(unlocks(&transport.calls), 1);
    assert_eq!(puts(&transport.calls), 0);
    transport.assert_finished();
}

#[test]
fn the_ask_policy_does_not_prompt_while_the_lock_is_held() {
    let mut transport = replay(
        FIXTURE,
        vec![
            fx(6),
            feed(NO_INACTIVE_FEED),
            fx(7),
            fx(6),
            feed(FOREIGN_INACTIVE_FEED),
            fx(9),
        ],
    );
    let terminal = FakeTerminal::answering(true);
    let result = DeployProg::run_classified_with(
        &mut transport,
        input(false),
        Some(&connection(ForeignInactive::Ask)),
        &CancelFlag::new(),
        &terminal,
    );

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(
        verdict(&result.outcome),
        format!("REJECTED: inactive version of another user exists: {OTHER}")
    );
    assert_eq!(terminal.prompts(), Vec::<String>::new());
    assert_eq!(unlocks(&transport.calls), 1);
    assert_eq!(puts(&transport.calls), 0);
    transport.assert_finished();
}

#[test]
fn the_policy_allow_overwrites_with_a_warning() {
    let cases = [
        (true, simulate_steps(FOREIGN_INACTIVE_FEED)),
        (
            false,
            vec![
                fx(6),
                feed(FOREIGN_INACTIVE_FEED),
                fx(7),
                fx(6),
                feed(FOREIGN_INACTIVE_FEED),
                fx(8),
                fx(17),
                fx(18),
                fx(19),
                fx(20),
            ],
        ),
    ];

    for (simulate, steps) in cases {
        let mut transport = replay(FIXTURE, steps);
        let outcome = DeployProg::run(
            &mut transport,
            input(simulate),
            Some(&connection(ForeignInactive::Allow)),
        );

        assert_eq!(outcome.exit, 0, "{simulate}");
        assert_eq!(
            outcome.stderr,
            format!("W: overwriting the inactive version of {OTHER} (policy allow)"),
            "{simulate}"
        );
        assert_eq!(locks(&transport.calls), 1, "{simulate}");
        assert_eq!(
            transport.calls[1].uri,
            versions_uri(),
            "the version list comes before the lock: {simulate}"
        );
        assert!(
            transport.calls[2].uri.contains("_action=LOCK"),
            "{simulate}"
        );
        transport.assert_finished();
    }
}

#[test]
fn the_ask_policy_prompts_and_continues_on_yes() {
    let mut transport = replay(FIXTURE, simulate_steps(FOREIGN_INACTIVE_FEED));
    let terminal = FakeTerminal::answering(true);
    let result = DeployProg::run_classified_with(
        &mut transport,
        input(true),
        Some(&connection(ForeignInactive::Ask)),
        &CancelFlag::new(),
        &terminal,
    );

    assert_eq!(result.outcome.status, Some(Status::SimulatedOk));
    assert_eq!(result.outcome.exit, 0);
    assert_eq!(
        terminal.prompts(),
        vec![format!(
            "YTEST_ABAPREACH01: inactive version of {OTHER} would be overwritten \
             (2 lines changed vs active). Overwrite? [y/N] "
        )]
    );
    assert_eq!(
        result.outcome.stderr,
        format!("W: overwriting the inactive version of {OTHER} (confirmed at the prompt)")
    );
    assert_eq!(locks(&transport.calls), 1);
    transport.assert_finished();
}

#[test]
fn the_ask_policy_reports_confirmation_in_the_warning() {
    let mut transport = replay(FIXTURE, simulate_steps(FOREIGN_INACTIVE_FEED));
    let terminal = FakeTerminal::answering(true);
    let result = DeployProg::run_classified_with(
        &mut transport,
        input(true),
        Some(&connection(ForeignInactive::Ask)),
        &CancelFlag::new(),
        &terminal,
    );

    assert_eq!(
        result.outcome.stderr,
        format!("W: overwriting the inactive version of {OTHER} (confirmed at the prompt)")
    );
    transport.assert_finished();
}

#[test]
fn the_ask_policy_rejects_anything_but_yes() {
    let mut transport = replay(FIXTURE, vec![fx(6), feed(FOREIGN_INACTIVE_FEED)]);
    let terminal = FakeTerminal::answering(false);
    let result = DeployProg::run_classified_with(
        &mut transport,
        input(true),
        Some(&connection(ForeignInactive::Ask)),
        &CancelFlag::new(),
        &terminal,
    );

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert_eq!(
        verdict(&result.outcome),
        format!(
            "REJECTED: inactive version of another user exists: {OTHER} (declined at the prompt)"
        )
    );
    assert_eq!(locks(&transport.calls), 0);
    transport.assert_finished();
}

#[test]
fn the_ask_policy_without_a_terminal_behaves_as_reject() {
    let mut transport = replay(FIXTURE, vec![fx(6), feed(FOREIGN_INACTIVE_FEED)]);
    let terminal = FakeTerminal::default();
    let result = DeployProg::run_classified_with(
        &mut transport,
        input(true),
        Some(&connection(ForeignInactive::Ask)),
        &CancelFlag::new(),
        &terminal,
    );

    assert_eq!(result.outcome.status, Some(Status::Rejected));
    assert_eq!(result.outcome.exit, 1);
    assert_eq!(
        verdict(&result.outcome),
        format!(
            "REJECTED: inactive version of another user exists: {OTHER} \
             (policy ask needs a terminal)"
        )
    );
    assert!(terminal.prompts().is_empty());
    assert_eq!(locks(&transport.calls), 0);
    transport.assert_finished();
}

#[test]
fn an_inactive_version_of_the_logon_user_is_no_obstacle() {
    for policy in [
        ForeignInactive::Reject,
        ForeignInactive::Allow,
        ForeignInactive::Ask,
    ] {
        let mut transport = replay(FIXTURE, simulate_steps(OWN_INACTIVE_FEED));
        let outcome = DeployProg::run(&mut transport, input(true), Some(&connection(policy)));

        assert_eq!(outcome.status, Some(Status::SimulatedOk), "{policy}");
        assert_eq!(outcome.exit, 0, "{policy}");
        assert_eq!(outcome.stderr, "", "{policy}");
        transport.assert_finished();
    }
}

#[test]
fn the_owner_comparison_ignores_case() {
    let mut connection = connection(ForeignInactive::Reject);

    connection.user = OTHER.to_ascii_lowercase();

    let mut transport = replay(FIXTURE, simulate_steps(FOREIGN_INACTIVE_FEED));
    let outcome = DeployProg::run(&mut transport, input(true), Some(&connection));

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(outcome.stderr, "");
    transport.assert_finished();
}

#[test]
fn a_version_list_without_an_inactive_version_is_no_obstacle() {
    let mut transport = replay(FIXTURE, simulate_steps(NO_INACTIVE_FEED));
    let outcome = DeployProg::run(
        &mut transport,
        input(true),
        Some(&connection(ForeignInactive::Reject)),
    );

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(outcome.stderr, "");
    transport.assert_finished();
}

#[test]
fn an_unreadable_version_list_fails_closed_unless_allowed() {
    for policy in [ForeignInactive::Reject, ForeignInactive::Ask] {
        let mut transport = replay(
            FIXTURE,
            vec![fx(6), Step::Versions(Ok(Response::empty(500)))],
        );
        let outcome = DeployProg::run(&mut transport, input(true), Some(&connection(policy)));

        assert_eq!(outcome.status, Some(Status::Rejected), "{policy}");
        assert_eq!(outcome.exit, 1, "{policy}");
        assert_eq!(
            verdict(&outcome),
            format!("REJECTED: version list unavailable: HTTP 500 (policy {policy})")
        );
        assert_eq!(locks(&transport.calls), 0);
        transport.assert_finished();
    }

    let mut transport = replay(
        FIXTURE,
        vec![
            fx(6),
            Step::Versions(Ok(Response::empty(500))),
            fx(7),
            fx(6),
            Step::Versions(Ok(Response::empty(500))),
            fx(8),
            fx(9),
        ],
    );
    let outcome = DeployProg::run(
        &mut transport,
        input(true),
        Some(&connection(ForeignInactive::Allow)),
    );

    assert_eq!(outcome.status, Some(Status::SimulatedOk));
    assert_eq!(
        outcome.stderr,
        "W: version list unavailable: HTTP 500; continuing (policy allow)"
    );
    transport.assert_finished();
}

#[test]
fn an_unparsable_version_list_fails_closed() {
    let mut transport = replay(
        FIXTURE,
        vec![fx(6), Step::Versions(Ok(Response::text(200, "<a><b></a>")))],
    );
    let outcome = DeployProg::run(
        &mut transport,
        input(true),
        Some(&connection(ForeignInactive::Reject)),
    );

    assert_eq!(outcome.status, Some(Status::Rejected));
    assert!(
        verdict(&outcome).starts_with("REJECTED: cannot parse the version list:"),
        "{}",
        outcome.stdout
    );
    assert_eq!(locks(&transport.calls), 0);
    transport.assert_finished();
}

#[test]
fn the_guard_report_names_the_policy() {
    let mut transport = replay(FIXTURE, simulate_steps(NO_INACTIVE_FEED));
    let selection = Selection::new(BackendKind::AdtHttp, SelectionReason::DefaultAdt);
    let result = DeployProg::run_selected(
        &mut transport,
        input(true),
        &selection,
        Some(&connection(ForeignInactive::Allow)),
        &CancelFlag::new(),
    );

    assert!(
        result.outcome.stdout.contains(
            "guards: SAP lock/CSRF/transport/authorization, base hash, \
             foreign inactive (policy allow)"
        ),
        "{}",
        result.outcome.stdout
    );
    assert!(result.outcome.stdout.contains(
        "not applied (FM guards): Z/Y name, productive client, system change option, SE06"
    ));
    assert!(
        !result
            .outcome
            .stdout
            .contains("inactive owned by another user")
    );
    transport.assert_finished();
}

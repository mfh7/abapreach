//! Guards for the native four-runner verification workflow. The workflow file is the artifact, so
//! the tests read it and check the parts a later edit could weaken: the trigger set, the runner
//! matrix, the four cargo commands in every runner, and the absence of anything that could reach a
//! real system or hide a failing suite.

use std::path::PathBuf;

const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";

/// The four release targets, exactly as the milestone contract lists them.
const RUNNERS: [(&str, &str); 4] = [
    ("macos-14", "arm64"),
    ("windows-latest", "x64"),
    ("ubuntu-24.04", "x64"),
    ("ubuntu-24.04-arm", "arm64"),
];

const FORMATTED: &str = "cargo fmt --check";
const LINTED: &str = "cargo clippy --all-targets --all-features -- -D warnings";
const DEFAULT_TESTS: &str = "cargo test";
const FAULT_TESTS: &str = "cargo test --features fault-injection";
const REQUIRED: [&str; 4] = [FORMATTED, LINTED, DEFAULT_TESTS, FAULT_TESTS];

/// Nothing here may reach a system: no secret, and no connection source.
const FORBIDDEN: [&str; 4] = [
    "secrets.",
    "ABAP_CONNECTIONS",
    "connections.local",
    "ABAPREACH_RFC_LIBRARY",
];

fn workflow_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(WORKFLOW_PATH);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn document() -> Block {
    parse(&workflow_text())
}

fn steps(document: &Block) -> &[Block] {
    items(get(get(get(document, "jobs"), "verify"), "steps"))
}

fn run_commands(document: &Block) -> Vec<String> {
    steps(document)
        .iter()
        .filter_map(|step| find(step, "run"))
        .flat_map(|script| lines(script).into_iter().map(str::to_owned))
        .collect()
}

/// The index of the step whose run script holds a line containing `needle`.
fn step_of(document: &Block, needle: &str) -> usize {
    steps(document)
        .iter()
        .position(|step| {
            find(step, "run")
                .map(|script| lines(script).iter().any(|line| line.contains(needle)))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no step runs a command containing `{needle}`"))
}

fn step_with_id(document: &Block, id: &str) -> usize {
    steps(document)
        .iter()
        .position(|step| find(step, "id").map(scalar) == Some(id))
        .unwrap_or_else(|| panic!("no step has id `{id}`"))
}

#[test]
fn ci_workflow_has_one_job_on_the_four_target_runners() {
    let document = document();
    assert_eq!(keys(get(&document, "jobs")), ["verify"]);

    let job = get(get(&document, "jobs"), "verify");
    assert_eq!(scalar(get(job, "runs-on")), "${{ matrix.runner.label }}");

    let matrix = get(get(job, "strategy"), "matrix");
    let runners: Vec<(&str, &str)> = items(get(matrix, "runner"))
        .iter()
        .map(|entry| (scalar(get(entry, "label")), scalar(get(entry, "arch"))))
        .collect();
    assert_eq!(runners, RUNNERS);
}

#[test]
fn ci_workflow_triggers_are_v_tags_and_manual_dispatch_only() {
    let document = document();
    let triggers = get(&document, "on");
    assert_eq!(keys(triggers), ["push", "workflow_dispatch"]);

    let push = get(triggers, "push");
    assert_eq!(keys(push), ["tags"]);
    assert_eq!(scalars(get(push, "tags")), ["v*"]);

    // A manual dispatch has no filter, so it stays usable for the rc gate from the tagged commit.
    assert_eq!(map(get(triggers, "workflow_dispatch")), &[]);
}

#[test]
fn ci_workflow_runs_formatting_linting_and_both_feature_suites() {
    let commands = run_commands(&document());
    for required in REQUIRED {
        assert!(
            commands.iter().any(|command| command == required),
            "`{required}` is missing; the workflow runs: {commands:?}"
        );
    }
}

#[test]
fn ci_workflow_never_skips_or_softens_a_required_command() {
    let document = document();
    for step in steps(&document) {
        let Some(script) = find(step, "run") else {
            continue;
        };
        let commands = lines(script);
        assert!(
            !keys(step).contains(&"continue-on-error"),
            "a step sets continue-on-error: {step:?}"
        );
        if !commands.iter().any(|command| REQUIRED.contains(command)) {
            continue;
        }
        assert!(
            !keys(step).contains(&"if"),
            "a required command is conditional, so a runner could skip it: {step:?}"
        );
        for command in commands {
            for forbidden in ["|| true", "|| exit 0", "--skip", "--exclude"] {
                assert!(
                    !command.contains(forbidden),
                    "`{command}` softens a failure with `{forbidden}`"
                );
            }
        }
    }
}

#[test]
fn ci_workflow_stays_offline_from_sap() {
    let text = workflow_text();
    for forbidden in FORBIDDEN {
        assert!(
            !text.contains(forbidden),
            "the workflow references `{forbidden}`, so a job could reach a system"
        );
    }

    let document = document();
    assert_eq!(keys(get(&document, "permissions")), ["contents"]);
    assert_eq!(
        scalar(get(get(&document, "permissions"), "contents")),
        "read"
    );
}

#[test]
fn ci_workflow_rejects_a_runner_of_the_wrong_architecture_before_building() {
    let document = document();
    let steps = steps(&document);
    let first = &steps[0];
    assert_eq!(scalar(get(first, "id")), "runner_is_available");
    assert!(
        find(first, "name").is_some(),
        "a failing gate needs a name in the UI"
    );

    let script = scalar(get(first, "run"));
    assert!(script.contains("matrix.runner.arch"), "{script}");
    assert!(script.contains("::error::"), "{script}");
    assert!(script.contains("exit 1"), "{script}");

    assert!(
        step_with_id(&document, "runner_is_available") < step_of(&document, FORMATTED),
        "the architecture check must come before the build steps"
    );
}

#[test]
fn ci_workflow_installs_nasm_before_the_windows_build() {
    let document = document();
    let steps_of_workflow = steps(&document);
    let nasm = step_of(&document, "nasm");
    assert!(
        nasm < step_of(&document, FORMATTED),
        "the NASM install must come before a step that builds aws-lc-sys"
    );
    assert_eq!(
        scalar(get(&steps_of_workflow[nasm], "if")),
        "runner.os == 'Windows'"
    );
}

/// The workflow is YAML in a subset: block mappings, block sequences and `|` block scalars.
#[derive(Debug, PartialEq)]
enum Block {
    Value(String),
    List(Vec<Block>),
    Map(Vec<(String, Block)>),
}

fn parse(text: &str) -> Block {
    let mut lines = Lines::new(text);
    lines.block(0)
}

struct Lines {
    lines: Vec<(usize, String)>,
    pos: usize,
}

impl Lines {
    fn new(text: &str) -> Self {
        let lines = text
            .lines()
            .filter_map(|line| {
                let content = line.trim();
                if content.is_empty() || content.starts_with('#') {
                    return None;
                }
                Some((line.len() - line.trim_start().len(), content.to_owned()))
            })
            .collect();
        Self { lines, pos: 0 }
    }

    fn indent(&self) -> Option<usize> {
        self.lines.get(self.pos).map(|(indent, _)| *indent)
    }

    fn content(&self) -> String {
        self.lines[self.pos].1.clone()
    }

    fn block(&mut self, indent: usize) -> Block {
        if self.content().starts_with("- ") || self.content() == "-" {
            self.list(indent)
        } else {
            self.mapping(indent)
        }
    }

    fn list(&mut self, indent: usize) -> Block {
        let mut found = Vec::new();
        while self.indent() == Some(indent)
            && (self.content().starts_with("- ") || self.content() == "-")
        {
            let item = self.content()[1..].trim().to_owned();
            self.pos += 1;
            if item.is_empty() {
                let nested = self.indent().unwrap_or(indent + 2);
                found.push(self.block(nested));
                continue;
            }
            let Some((key, rest)) = split_entry(&item) else {
                found.push(Block::Value(unquote(&item)));
                continue;
            };
            // `- key: value` opens a mapping whose other keys sit two columns further right.
            let entry_indent = indent + 2;
            let mut entries = vec![(key, self.entry(&rest, entry_indent))];
            while self.indent() == Some(entry_indent) {
                let text = self.content();
                let Some((key, rest)) = split_entry(&text) else {
                    break;
                };
                self.pos += 1;
                entries.push((key, self.entry(&rest, entry_indent)));
            }
            found.push(Block::Map(entries));
        }
        Block::List(found)
    }

    fn mapping(&mut self, indent: usize) -> Block {
        let mut entries = Vec::new();
        while self.indent() == Some(indent) {
            let text = self.content();
            let Some((key, rest)) = split_entry(&text) else {
                break;
            };
            self.pos += 1;
            entries.push((key, self.entry(&rest, indent)));
        }
        Block::Map(entries)
    }

    /// What follows `key:` in the entry just at `self.pos`: an inline value, a block scalar, a
    /// nested block, or nothing at all.
    fn entry(&mut self, rest: &str, indent: usize) -> Block {
        if rest.starts_with('|') || rest.starts_with('>') {
            return Block::Value(self.block_scalar(indent));
        }
        if !rest.is_empty() {
            return Block::Value(unquote(rest));
        }
        match self.indent() {
            Some(nested) if nested > indent => self.block(nested),
            _ => Block::Map(Vec::new()),
        }
    }

    fn block_scalar(&mut self, indent: usize) -> String {
        let mut body = Vec::new();
        while let Some(nested) = self.indent() {
            if nested <= indent {
                break;
            }
            body.push(self.content());
            self.pos += 1;
        }
        body.join("\n")
    }
}

fn split_entry(text: &str) -> Option<(String, String)> {
    let at = text.find(':')?;
    let rest = &text[at + 1..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((text[..at].trim().to_owned(), rest.trim().to_owned()))
}

fn unquote(text: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    text.to_owned()
}

fn map(block: &Block) -> &[(String, Block)] {
    match block {
        Block::Map(entries) => entries,
        other => panic!("expected a mapping, found {other:?}"),
    }
}

fn get<'a>(block: &'a Block, key: &str) -> &'a Block {
    find(block, key).unwrap_or_else(|| panic!("missing key `{key}` in {block:?}"))
}

fn find<'a>(block: &'a Block, key: &str) -> Option<&'a Block> {
    map(block)
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn keys(block: &Block) -> Vec<&str> {
    map(block)
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
}

fn items(block: &Block) -> &[Block] {
    match block {
        Block::List(items) => items.as_slice(),
        other => panic!("expected a list, found {other:?}"),
    }
}

fn scalar(block: &Block) -> &str {
    match block {
        Block::Value(text) => text,
        other => panic!("expected a value, found {other:?}"),
    }
}

fn scalars(block: &Block) -> Vec<&str> {
    items(block).iter().map(scalar).collect()
}

fn lines(block: &Block) -> Vec<&str> {
    scalar(block)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
fn ci_workflow_confirms_the_fault_injection_suite_is_not_empty() {
    let document = document();
    let guard = step_of(&document, "--test fault_injection_test");
    assert!(
        guard > step_of(&document, FAULT_TESTS),
        "the empty-suite check must follow the fault-injection run"
    );

    let script = scalar(get(&steps(&document)[guard], "run"));
    assert!(script.contains("--list"), "{script}");
    assert!(script.contains("-lt 1"), "{script}");
    assert!(script.contains("exit 1"), "{script}");
}

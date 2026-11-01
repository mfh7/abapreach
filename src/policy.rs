//! Write-guard policies from the `policies` block of `connections.json`.
//!
//! The block is one object per guard, so a later guard adds a key instead of a new section:
//! a top-level `policies` sets the value for every server and `servers[].policies` overrides
//! single keys. An unknown guard name or an unknown value stops the run at load time.

use serde::Deserialize;
use std::fmt::Display;
use std::io::{IsTerminal, Write, stderr, stdin};

/// What `foreign_inactive` does when the inactive version belongs to another user.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ForeignInactive {
    /// Refuse before the lock.
    Reject,
    /// Write anyway and print a warning.
    Allow,
    /// Ask on the terminal; no terminal counts as `reject`.
    Ask,
}

impl ForeignInactive {
    pub fn token(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::Allow => "allow",
            Self::Ask => "ask",
        }
    }
}

impl Display for ForeignInactive {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.token())
    }
}

/// The effective policies of one connection: every guard has a value, `reject` unless configured.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Policies {
    pub foreign_inactive: ForeignInactive,
}

impl Default for Policies {
    fn default() -> Self {
        Self {
            foreign_inactive: ForeignInactive::Reject,
        }
    }
}

/// One `policies` object as written in the configuration; an absent key keeps the outer value.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    #[serde(default)]
    foreign_inactive: Option<ForeignInactive>,
}

impl PolicyDocument {
    pub fn apply(&self, outer: &Policies) -> Policies {
        Policies {
            foreign_inactive: self.foreign_inactive.unwrap_or(outer.foreign_inactive),
        }
    }
}

/// What the version list said about the inactive version of the program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InactiveVersion<'a> {
    /// There is no inactive version.
    Absent,
    /// The inactive version belongs to this user (SAP reports the author of the version).
    Owned { owner: &'a str },
    /// The version list could not be read, so the state of the inactive version is unknown.
    Unknown { reason: &'a str },
}

/// The summary the `ask` policy shows with its prompt: stdout keeps the diff until the process
/// ends, so without it the user answers with the change still invisible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Prompt<'a> {
    /// The object the deploy writes.
    pub name: &'a str,
    /// The lines the deploy changes against the active version.
    pub changed: usize,
}

impl Prompt<'_> {
    /// The one line the user answers on, naming the object, the owner and the size of the change.
    fn summary(&self, owner: &str) -> String {
        format!(
            "{}: inactive version of {} would be overwritten ({} lines changed vs active). \
             Overwrite? [y/N] ",
            self.name, owner, self.changed
        )
    }
}

/// What one guard decided: continue the deploy, or refuse it before anything is locked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// Continue; the optional line is a `W:` warning for stderr.
    Continue(Option<String>),
    /// Refuse; the reason is printed as `REJECTED: <reason>` with exit 1, without a lock.
    Refuse(String),
}

/// The terminal the `ask` policy prompts on: the prompt goes to stderr, the answer comes from
/// stdin. Tests replace it to answer without a terminal.
pub trait Confirmer {
    /// Whether the prompt can be shown and answered at all.
    fn interactive(&self) -> bool;

    /// Shows `prompt` and reads one answer; only `y` and `yes` count as yes.
    fn confirm(&self, prompt: &str) -> bool;
}

/// The real terminal of the process.
#[derive(Clone, Copy, Debug, Default)]
pub struct Terminal;

impl Confirmer for Terminal {
    fn interactive(&self) -> bool {
        stdin().is_terminal() && stderr().is_terminal()
    }

    fn confirm(&self, prompt: &str) -> bool {
        eprint!("{prompt}");
        let _ = stderr().flush();
        let mut answer = String::new();

        match stdin().read_line(&mut answer) {
            Ok(_) => matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
            Err(_) => false,
        }
    }
}

impl Policies {
    /// The `foreign_inactive` guard: another user's inactive version is overwritten by the next
    /// deploy, so the policy decides whether that happens, and an unreadable version list fails
    /// closed unless the policy is `allow`.
    ///
    /// `prompt` describes the deploy for the `ask` policy and is `None` when the guard runs while
    /// the lock is held: an answer would come with the object locked, so `ask` then refuses like
    /// `reject` instead of prompting.
    pub fn inactive_version(
        &self,
        state: InactiveVersion<'_>,
        user: &str,
        prompt: Option<Prompt<'_>>,
        confirmer: &dyn Confirmer,
    ) -> Decision {
        let policy = self.foreign_inactive;

        match state {
            InactiveVersion::Absent => Decision::Continue(None),
            InactiveVersion::Owned { owner } if same_user(owner, user) => Decision::Continue(None),
            InactiveVersion::Owned { owner } => match policy {
                ForeignInactive::Reject => Decision::Refuse(exists(owner)),
                ForeignInactive::Allow => Decision::Continue(Some(overwrite(owner))),
                ForeignInactive::Ask => ask(owner, prompt, confirmer),
            },
            InactiveVersion::Unknown { reason } => match policy {
                ForeignInactive::Allow => {
                    Decision::Continue(Some(format!("W: {reason}; continuing (policy allow)")))
                }
                policy => Decision::Refuse(format!("{reason} (policy {policy})")),
            },
        }
    }
}

fn exists(owner: &str) -> String {
    format!("inactive version of another user exists: {owner}")
}

fn overwrite(owner: &str) -> String {
    format!("W: overwriting the inactive version of {owner} (policy allow)")
}

fn ask(owner: &str, prompt: Option<Prompt<'_>>, confirmer: &dyn Confirmer) -> Decision {
    let Some(prompt) = prompt else {
        return Decision::Refuse(exists(owner));
    };

    if !confirmer.interactive() {
        return Decision::Refuse(format!("{} (policy ask needs a terminal)", exists(owner)));
    }

    if confirmer.confirm(&prompt.summary(owner)) {
        return Decision::Continue(Some(overwrite(owner)));
    }

    Decision::Refuse(format!("{} (declined at the prompt)", exists(owner)))
}

fn same_user(owner: &str, user: &str) -> bool {
    owner.eq_ignore_ascii_case(user)
}

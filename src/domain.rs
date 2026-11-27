#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Active,
    SimulatedOk,
    Inactive,
    Rejected,
    Partial,
}

impl Status {
    pub fn exit(self) -> i32 {
        match self {
            Self::Active | Self::SimulatedOk => 0,
            Self::Inactive | Self::Rejected | Self::Partial => 1,
        }
    }
}

/// Who runs an operation. The CLI names its flags and shows hashes; an MCP call has no flags, and
/// its client may not be allowed to read the source, so its texts name neither.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Origin {
    #[default]
    Cli,
    Mcp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: Option<Status>,
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
}

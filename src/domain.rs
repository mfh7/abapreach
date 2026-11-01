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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: Option<Status>,
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
}

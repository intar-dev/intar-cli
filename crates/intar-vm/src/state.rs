use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VmState {
    Starting,
    Booting,
    CloudInit,
    Ready,
    Error,
}

impl VmState {
    #[must_use]
    pub fn step(&self) -> (u32, u32) {
        match self {
            VmState::Starting => (1, 4),
            VmState::Booting => (2, 4),
            VmState::CloudInit => (3, 4),
            VmState::Ready => (4, 4),
            VmState::Error => (0, 4),
        }
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            VmState::Starting => "Starting",
            VmState::Booting => "Booting",
            VmState::CloudInit => "Cloud-init",
            VmState::Ready => "Ready",
            VmState::Error => "Error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScenarioState {
    Initializing,
    Running,
    Completed,
    Error,
}

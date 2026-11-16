//! Knowledge agents' link to the daemon's settings: a session asks it before a knowledge
//! agent answers whether the model the agent ran on is still switched on.

use std::sync::Weak;

use crate::state::DaemonState;

/// Refuses questions for a model the user switched off since the agent was kept.
#[derive(Debug)]
pub(crate) struct ModelSwitch {
    state: Weak<DaemonState>,
}

impl ModelSwitch {
    pub(crate) fn new(state: Weak<DaemonState>) -> Self {
        ModelSwitch { state }
    }
}

impl imperium_engine::ModelAvailability for ModelSwitch {
    fn unavailable(&self, spec: &str) -> Option<String> {
        let state = self.state.upgrade()?;
        state.settings().models.is_disabled(spec).then(|| {
            "it is turned off (\"Use this model\" in Settings, Models); turn it on to ask this \
             agent, or retire the agent"
                .to_string()
        })
    }
}

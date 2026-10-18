//! The agent and models picked last time, so the start screen defaults to them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::daemon;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Prefs {
    pub agent: Agent,
    /// The model last picked for each agent. Missing means the agent's default.
    pub models: BTreeMap<Agent, String>,
}

impl Prefs {
    /// The saved preferences, or the defaults if there are none or they cannot be read.
    pub fn load() -> Prefs {
        file()
            .ok()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::write(file()?, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn model(&self, agent: Agent) -> Option<&str> {
        self.models.get(&agent).map(String::as_str)
    }
}

fn file() -> Result<PathBuf> {
    Ok(daemon::data_dir()?.join("prefs.json"))
}

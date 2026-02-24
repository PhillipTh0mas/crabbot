use std::{collections::HashMap, path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{Error, Result};

use super::types::{AgentId, AgentProfile, AgentProfilePatch, MdInclude, ToolPolicy};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentIndex {
    pub agents: HashMap<AgentId, AgentProfile>,
}

#[derive(Debug)]
pub struct AgentRegistry {
    path: PathBuf,
    index: RwLock<AgentIndex>,
    file_mtime: RwLock<Option<SystemTime>>,
}

impl AgentRegistry {
    /// `path` should be something like `<data_dir>/agents/`
    pub async fn open(folder: PathBuf) -> Result<Self> {
        let path = folder.join("agents.json");
        let (idx, mtime) = if path.exists() {
            let md = std::fs::metadata(&path).map_err(|e| Error::io(e.to_string()))?;
            let mtime = md.modified().ok();

            let data = tokio::fs::read_to_string(&path).await?;
            let mut idx: AgentIndex =
                serde_json::from_str(&data).map_err(|e| Error::io(e.to_string()))?;

            ensure_default_agents(&mut idx)?;

            (idx, mtime)
        } else {
            let mut idx = AgentIndex::default();
            ensure_default_agents(&mut idx)?;

            // Create registry with defaults and persist immediately so restarts work.
            let reg = Self {
                path,
                index: RwLock::new(idx),
                file_mtime: RwLock::new(None),
            };
            reg.save().await?;
            return Ok(reg);
        };

        Ok(Self {
            path,
            index: RwLock::new(idx),
            file_mtime: RwLock::new(mtime),
        })
    }

    pub async fn list(&self) -> Vec<AgentProfile> {
        let idx = self.index.read().await;
        let mut out: Vec<_> = idx.agents.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub async fn get(&self, id: &str) -> Option<AgentProfile> {
        let idx = self.index.read().await;
        // print agent ids
        tracing::info!("Agent IDs: {:?}", idx.agents.keys());
        idx.agents.get(id).cloned()
    }

    /// Create or replace (idempotent). Persists to disk.
    pub async fn upsert(&self, mut agent: AgentProfile, now_ts_ms: i64) -> Result<AgentProfile> {
        let mut idx = self.index.write().await;

        let exists = idx.agents.contains_key(&agent.id);
        if !exists {
            agent.created_ts_ms = if agent.created_ts_ms == 0 {
                now_ts_ms
            } else {
                agent.created_ts_ms
            };
            agent.version = if agent.version == 0 { 1 } else { agent.version };
        } else {
            let prev = idx.agents.get(&agent.id).cloned();
            if let Some(prev) = prev {
                agent.created_ts_ms = prev.created_ts_ms;
                agent.version = prev.version.saturating_add(1);
            } else {
                agent.version = agent.version.saturating_add(1).max(1);
            }
        }

        agent.updated_ts_ms = now_ts_ms;

        idx.agents.insert(agent.id.clone(), agent.clone());
        drop(idx);

        self.save().await?;
        Ok(agent)
    }

    /// Patch an existing agent. Persists to disk.
    pub async fn patch(
        &self,
        id: &str,
        patch: AgentProfilePatch,
        now_ts_ms: i64,
    ) -> Result<AgentProfile> {
        let mut idx = self.index.write().await;
        let agent = idx
            .agents
            .get_mut(id)
            .ok_or_else(|| Error::other(format!("agent not found: {id}")))?;

        if let Some(v) = patch.display_name {
            agent.display_name = v;
        }
        if let Some(v) = patch.description {
            agent.description = v;
        }
        if let Some(v) = patch.md_includes {
            agent.md_includes = v;
        }

        if let Some(v) = patch.tool_policy {
            agent.tool_policy = v;
        }
        if let Some(v) = patch.tools {
            agent.tools = v;
        }

        if let Some(v) = patch.enable_recall {
            agent.enable_recall = v;
        }
        if let Some(v) = patch.recall_top_k {
            agent.recall_top_k = v;
        }
        if let Some(v) = patch.recall_max_chars {
            agent.recall_max_chars = v;
        }

        if let Some(v) = patch.max_steps {
            agent.max_steps = v;
        }

        if let Some(v) = patch.model {
            agent.model = v;
        }
        if let Some(v) = patch.temperature {
            agent.temperature = v;
        }
        if let Some(v) = patch.max_output_tokens {
            agent.max_output_tokens = v;
        }

        if let Some(v) = patch.enabled {
            agent.enabled = v;
        }

        agent.updated_ts_ms = now_ts_ms;
        agent.version = agent.version.saturating_add(1).max(1);

        let out = agent.clone();
        drop(idx);

        self.save().await?;
        Ok(out)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        // Prevent deleting required defaults (you can relax this later if you want).
        if id == "default" || id == "system" {
            return Err(Error::other(format!("cannot delete built-in agent: {id}")));
        }

        let mut idx = self.index.write().await;
        let existed = idx.agents.remove(id).is_some();
        drop(idx);

        if !existed {
            return Err(Error::other(format!("agent not found: {id}")));
        }
        self.save().await
    }

    /// Reload from disk if file changed. Useful if you edit agents.json externally.
    pub async fn reload_if_changed(&self) -> Result<bool> {
        if !self.path.exists() {
            return Ok(false);
        }

        let md = std::fs::metadata(&self.path).map_err(|e| Error::io(e.to_string()))?;
        let mtime = match md.modified() {
            Ok(m) => m,
            Err(_) => return Ok(false),
        };

        let mut last = self.file_mtime.write().await;
        if last.is_some() && last.unwrap() == mtime {
            return Ok(false);
        }

        let data = tokio::fs::read_to_string(&self.path).await?;
        let mut idx: AgentIndex =
            serde_json::from_str(&data).map_err(|e| Error::io(e.to_string()))?;

        ensure_default_agents(&mut idx)?;

        {
            let mut g = self.index.write().await;
            *g = idx;
        }

        *last = Some(mtime);
        Ok(true)
    }

    pub async fn save(&self) -> Result<()> {
        let snapshot = { self.index.read().await.clone() };

        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| Error::io(e.to_string()))?;

        let tmp = self.path.with_extension("tmp");
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &self.path).await?;

        if let Ok(md) = std::fs::metadata(&self.path) {
            if let Ok(mtime) = md.modified() {
                let mut last = self.file_mtime.write().await;
                *last = Some(mtime);
            }
        }

        Ok(())
    }
}

fn ensure_default_agents(idx: &mut AgentIndex) -> Result<()> {
    let now = crate::time::now_ts_ms();

    idx.agents
        .entry("default".to_string())
        .or_insert_with(|| AgentProfile {
            id: "default".to_string(),
            display_name: "Default Agent".to_string(),
            description: "Default user-facing agent".to_string(),
            md_includes: vec![MdInclude::Sould],
            tool_policy: ToolPolicy::DenyList,
            // Empty means "no filtering" in tool filtering logic.
            tools: vec![],
            enable_recall: true,
            recall_top_k: None,
            recall_max_chars: None,
            max_steps: Some(16),
            model: None,
            temperature: None,
            max_output_tokens: None,
            created_ts_ms: now,
            updated_ts_ms: now,
            version: 1,
            enabled: true,
            include_long_term_memory: true,
            include_daily_memory_days: 2,
            memory_prompt_max_chars: 1000,
        });

    idx.agents
        .entry("system".to_string())
        .or_insert_with(|| AgentProfile {
            id: "system".to_string(),
            display_name: "System".to_string(),
            description: "Background maintenance agent".to_string(),
            md_includes: vec![MdInclude::Sould],
            tool_policy: ToolPolicy::DenyList,
            // Keep restrictive by default; add task tools later.
            tools: vec![],
            enable_recall: false,
            recall_top_k: None,
            recall_max_chars: None,
            max_steps: Some(8),
            model: None,
            temperature: Some(0.1),
            max_output_tokens: None,
            created_ts_ms: now,
            updated_ts_ms: now,
            version: 1,
            enabled: true,
            include_long_term_memory: true,
            include_daily_memory_days: 2,
            memory_prompt_max_chars: 1000,
        });

    Ok(())
}

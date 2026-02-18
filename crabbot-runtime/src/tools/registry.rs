// crates/crabbot-runtime/src/tools/registry.rs

use std::{collections::BTreeMap, sync::Arc};

use tokio::sync::RwLock;

use crate::tools::builtin::command::CommandTool;
use crate::tools::builtin::noop::NoopTool;
use crate::tools::tool::ToolSpec;

#[derive(Debug, Clone)]
pub struct CompactToolSpec {
    pub name: String,
    pub description: String,
}

#[derive(Debug)]
pub struct ToolRegistry {
    policy: crate::config::ToolPolicy,
    // name -> tool
    tools: RwLock<BTreeMap<String, Arc<dyn crate::tools::tool::Tool>>>,
}

impl ToolRegistry {
    pub fn new(policy: crate::config::ToolPolicy) -> Self {
        Self {
            policy,
            tools: RwLock::new(BTreeMap::new()),
        }
    }

    pub fn policy(&self) -> &crate::config::ToolPolicy {
        &self.policy
    }

    /// Global switch: if false, tools should not be presented to the model and should not execute.
    pub fn is_enabled(&self) -> bool {
        self.policy.enable_tools
    }

    /// Returns true if an allowlist is configured (used only for diagnostics / prompt hints).
    pub fn has_allowlist(&self) -> bool {
        self.policy
            .allowed_tools
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Central policy hook: is a tool name allowed to exist / be used?
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        if !self.is_enabled() {
            return false;
        }

        match self.policy.allowed_tools.as_ref() {
            None => true,
            Some(list) => list.iter().any(|t| t == tool_name),
        }
    }

    pub async fn register(
        &self,
        tool: Arc<dyn crate::tools::tool::Tool>,
    ) -> crate::error::Result<()> {
        let name = tool.name().to_string();

        // IMPORTANT: do not even register tools that aren't allowed.
        if !self.is_allowed(&name) {
            return Ok(());
        }

        let mut map = self.tools.write().await;
        map.insert(name, tool);
        Ok(())
    }

    pub async fn get(&self, name: &str) -> Option<Arc<dyn crate::tools::tool::Tool>> {
        let map = self.tools.read().await;
        map.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<String> {
        let map = self.tools.read().await;
        map.keys().cloned().collect()
    }

    pub async fn clear(&self) {
        let mut map = self.tools.write().await;
        map.clear();
    }

    // /// Compact specs for prompt listing. Only returns tools the model is allowed to use.
    pub async fn specs_compact(&self) -> Vec<CompactToolSpec> {
        if !self.is_enabled() {
            return vec![];
        }

        let map = self.tools.read().await;
        map.iter()
            .filter_map(|(name, tool)| {
                if !self.is_allowed(name) {
                    return None;
                }
                Some(CompactToolSpec {
                    name: name.to_string(),
                    description: tool.spec().description,
                })
            })
            .collect()
    }

    /// Compact specs for prompt listing. Only returns tools the model is allowed to use.
    pub async fn tool_specs(&self) -> Vec<ToolSpec> {
        if !self.is_enabled() {
            return vec![];
        }

        let map = self.tools.read().await;
        map.iter()
            .filter_map(|(name, tool)| {
                if !self.is_allowed(name) {
                    return None;
                }
                Some(tool.spec())
            })
            .collect()
    }

    // Built-in tools are always available unless policy blocks them.
    pub async fn register_builtins(&self) -> crate::error::Result<()> {
        // Wire your actual builtins here.
        // add noop tool
        let tool = Arc::new(NoopTool::new());
        self.register(tool).await?;
        // add command tool
        let tool = Arc::new(CommandTool::new());
        self.register(tool).await?;
        Ok(())
    }

    // Optional tools are feature/permission gated via policy.
    pub async fn register_optional(&self) -> crate::error::Result<()> {
        if !self.optional_enabled() {
            return Ok(());
        }
        Ok(())
    }

    fn optional_enabled(&self) -> bool {
        true
    }
}

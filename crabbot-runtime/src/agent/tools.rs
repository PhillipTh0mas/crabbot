use crate::agent::types::{AgentProfile, ToolPolicy};
use crate::tools::tool::ToolSpec;

pub fn filter_tool_specs(agent: &AgentProfile, all: Vec<ToolSpec>) -> Vec<ToolSpec> {
    if agent.tools.is_empty() {
        return all;
    }

    match agent.tool_policy {
        ToolPolicy::AllowList => all
            .into_iter()
            .filter(|t| agent.tools.iter().any(|name| name == &t.name))
            .collect(),

        ToolPolicy::DenyList => all
            .into_iter()
            .filter(|t| !agent.tools.iter().any(|name| name == &t.name))
            .collect(),
    }
}

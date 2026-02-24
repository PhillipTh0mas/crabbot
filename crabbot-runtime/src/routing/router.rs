// src/routing/router.rs
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct Route {
    pub agent_id: String,
    /// The session key without agent prefix (optional, but useful).
    pub session_key: String,
}

#[derive(Debug)]
pub struct DefaultSessionRouter {
    default_agent_id: String,
}

impl DefaultSessionRouter {
    pub fn new(cfg: crate::config::RoutingConfig) -> Self {
        Self {
            default_agent_id: cfg.default_agent_id,
        }
    }

    /// Resolve agent + normalized session_key from an incoming session_key.
    ///
    /// Convention:
    /// - "<agent_id>:<session_key>" => agent_id + session_key
    /// - "<session_key>" => default agent
    pub fn route(&self, incoming_session_key: &str) -> Result<Route> {
        if let Some((prefix, rest)) = incoming_session_key.split_once(':') {
            let agent = prefix.trim();
            let key = rest.trim();
            if !agent.is_empty() && !key.is_empty() {
                return Ok(Route {
                    agent_id: agent.to_string(),
                    session_key: key.to_string(),
                });
            }
        }

        Ok(Route {
            agent_id: self.default_agent_id.clone(),
            session_key: incoming_session_key.to_string(),
        })
    }
}

// crates/crabbot-runtime/src/config.rs

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub paths: Paths,
    pub api: ApiConfig,
    pub llm: LlmConfig,
    pub prompt: PromptConfig,
    pub queue: QueueConfig,
    pub routing: RoutingConfig,
    pub tool_policy: ToolPolicy,
    pub memory: MemoryConfig,

    pub idle_reset_after_ms: Option<i64>,
}

impl Config {
    pub fn load() -> Result<Self> {
        // laod from config fir if not present laod from env and store

        let paths = Paths::from_env()?;

        // try load
        let path = paths.config_file();
        let config = match path.exists() {
            true => {
                let config = serde_json::from_str(&std::fs::read_to_string(path)?)
                    .map_err(|e| Error::config(e.to_string()))?;
                config
            }
            false => {
                let config = Self {
                    paths,
                    api: ApiConfig::from_env()?,
                    llm: LlmConfig::from_env()?,
                    prompt: PromptConfig::from_env(),
                    queue: QueueConfig::from_env(),
                    routing: RoutingConfig::from_env(),
                    tool_policy: ToolPolicy::from_env(),
                    memory: MemoryConfig::from_env(),
                    idle_reset_after_ms: Some(env_i64("CRABBOT_IDLE_RESET_AFTER_MS", 60000)),
                };
                config.ensure_dirs()?;
                config.save()?;
                config
            }
        };

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.paths.config_file();
        let config =
            serde_json::to_string_pretty(self).map_err(|e| Error::config(e.to_string()))?;
        std::fs::write(path, config)?;
        Ok(())
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        self.paths.ensure_dirs()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Paths {
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn get_config_dir() -> Result<PathBuf> {
        // first check if we have std::env::var("CRABBOT_DATA_DIR")
        if let Ok(data_dir) = std::env::var("CRABBOT_DATA_DIR") {
            let data_dir = PathBuf::from(&data_dir);
            if data_dir.is_absolute() {
                return Ok(data_dir);
            }
        }

        // Check for SUDO_USER (Unix only - Windows doesn't have sudo)
        #[cfg(unix)]
        if let Ok(sudo_user) = std::env::var("SUDO_USER") {
            let home = homedir::unix::home(&sudo_user)
                .ok()
                .flatten()
                .ok_or(Error::config("Failed to get sudo user's home directory"))?;

            // On Linux, respect XDG_CONFIG_HOME if set to absolute path
            #[cfg(target_os = "linux")]
            if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
                let xdg_path = PathBuf::from(&xdg);
                if xdg_path.is_absolute() {
                    return Ok(xdg_path);
                }
            }

            // Platform-specific defaults (matching dirs crate behavior)
            #[cfg(target_os = "macos")]
            return Ok(home.join("Library/Application Support"));

            #[cfg(not(target_os = "macos"))]
            return Ok(home.join(".config"));
        }

        // No sudo or Windows - use standard dirs crate
        dirs::config_dir().ok_or(Error::config("Failed to get config directory"))
    }

    pub fn from_env() -> Result<Self> {
        let data_dir = Paths::get_config_dir()?;
        // append crabbot
        let data_dir = data_dir.join("crabbot");
        Ok(Self { data_dir })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.data_dir())?;
        std::fs::create_dir_all(self.sessions_dir())?;
        std::fs::create_dir_all(self.transcripts_dir())?;
        std::fs::create_dir_all(self.runtime_dir())?;
        std::fs::create_dir_all(self.memory_dir())?;

        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        let config_file = self.data_dir.join("config.toml");
        config_file
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    pub fn transcripts_dir(&self) -> PathBuf {
        self.data_dir.join("transcripts")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.data_dir.join("runtime")
    }

    pub fn sessions_index(&self) -> PathBuf {
        self.data_dir.join("sessions.json")
    }

    pub fn memory_dir(&self) -> PathBuf {
        self.data_dir.join("memory")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiConfig {
    pub bind: SocketAddr,
    pub auth_token: String,
}

impl ApiConfig {
    fn from_env() -> Result<Self> {
        let bind = std::env::var("CRABBOT_API_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8789".into())
            .parse()
            .map_err(|e| crate::error::Error::config(format!("invalid bind addr: {e}")))?;

        let auth_token =
            std::env::var("CRABBOT_AUTH_TOKEN").unwrap_or_else(|_| "dev-token-change-me".into());

        Ok(Self { bind, auth_token })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LLMProvider {
    OpenAI,
    Anthropic,
    Ollama,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmConfig {
    /// OpenAI-compatible base URL
    pub base_url: String,
    pub provider: LLMProvider,
    pub model: String,
    pub timeout_secs: u64,
    pub temperature: f32,
    pub max_output_tokens: u32,
}

impl LlmConfig {
    fn from_env() -> Result<Self> {
        Ok(Self {
            base_url: std::env::var("CRABBOT_LLM_BASE_URL")
                .unwrap_or_else(|_| "http://192.168.2.87:11434/v1".into()),
            provider: LLMProvider::Ollama,
            model: std::env::var("CRABBOT_LLM_MODEL").unwrap_or_else(|_| "glm-4.7-flash".into()),
            timeout_secs: env_u64("CRABBOT_LLM_TIMEOUT_SECS", 60),
            temperature: env_f32("CRABBOT_LLM_TEMPERATURE", 0.2),
            max_output_tokens: env_u32("CRABBOT_LLM_MAX_OUTPUT_TOKENS", 1024),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptConfig {
    pub max_history_events: usize,
    pub compaction_threshold_tokens: usize,
    pub compaction_max_chars: usize,
    pub flush_threshold_tokens: usize,
    pub system_preamble: String,
}

impl PromptConfig {
    fn from_env() -> Self {
        Self {
            max_history_events: env_usize("CRABBOT_PROMPT_MAX_HISTORY_EVENTS", 80),
            compaction_threshold_tokens: env_usize("CRABBOT_PROMPT_COMPACTION_MAX_TOKENS", 30000),
            compaction_max_chars: env_usize("CRABBOT_PROMPT_COMPACTION_MAX_CHARS", 350),
            flush_threshold_tokens: env_usize("CRABBOT_PROMPT_FLUSH_MAX_TOKENS", 10000),
            system_preamble: std::env::var("CRABBOT_SYSTEM_PROMPT").unwrap_or_else(|_| {
                "You are CrabBot. Use tools explicitly and safely.".to_string()
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueConfig {
    pub max_parallel_runs: usize,
}

impl QueueConfig {
    fn from_env() -> Self {
        Self {
            max_parallel_runs: env_usize("CRABBOT_MAX_PARALLEL_RUNS", 4),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// If true, all inbound messages collapse into a single session.
    pub single_session: bool,
}

impl RoutingConfig {
    fn from_env() -> Self {
        Self {
            single_session: env_bool("CRABBOT_SINGLE_SESSION", true),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolPolicy {
    /// If false, any tool invocation fails fast.
    pub enable_tools: bool,

    /// Optional allowlist of tool names.
    pub allowed_tools: Option<Vec<String>>,
}

impl ToolPolicy {
    fn from_env() -> Self {
        let allowed_tools = std::env::var("CRABBOT_ALLOWED_TOOLS").ok().map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        });

        Self {
            enable_tools: env_bool("CRABBOT_ENABLE_TOOLS", true),
            allowed_tools,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub embed_dim: usize,
    pub embed_model: String,
    pub model_provider_base_url: String,
    pub embed_model_provider: LLMProvider,

    pub chunk_max_chars: usize,
    pub flush_context_max_chars: usize,
    pub chunk_overlap_chars: usize,

    pub default_top_k: usize,
    pub recall_top_k: usize,
    pub recall_max_chars: usize,
    pub max_get_chars: usize,
}

impl MemoryConfig {
    pub fn from_env() -> Self {
        Self {
            embed_dim: env_usize("CRABBOT_EMBED_DIM", 768),
            model_provider_base_url: env_string(
                "CRABBOT_EMBED_MODEL_PROVIDER_BASE_URL",
                "http://192.168.2.87:11434/v1".to_string(),
            ),
            embed_model: env_string(
                "CRABBOT_EMBED_MODEL",
                "nnomic-embed-text-v2-moe".to_string(),
            ),
            embed_model_provider: LLMProvider::Ollama,
            chunk_max_chars: env_usize("CRABBOT_CHUNK_MAX_CHARS", 1200),
            flush_context_max_chars: env_usize("CRABBOT_FLUSH_CONTEXT_MAX_CHARS", 1500),
            chunk_overlap_chars: env_usize("CRABBOT_CHUNK_OVERLAP_CHARS", 200),
            default_top_k: env_usize("CRABBOT_DEFAULT_TOP_K", 4),
            recall_top_k: env_usize("CRABBOT_RECALL_TOP_K", 4),
            recall_max_chars: env_usize("CRABBOT_RECALL_MAX_CHARS", 1200),
            max_get_chars: env_usize("CRABBOT_MAX_GET_CHARS", 60_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    LongTerm,
    Daily,
}

impl MemoryKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MemoryKind::LongTerm => "long_term",
            MemoryKind::Daily => "daily",
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_string(key: &str, default: String) -> String {
    std::env::var(key).ok().unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .and_then(|v| match v.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" => Some(true),
            "0" | "false" | "FALSE" | "no" | "NO" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

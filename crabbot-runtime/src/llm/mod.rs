use std::sync::Arc;

use crate::{
    config::{LLMProvider, LlmConfig, PromptConfig},
    error::{Error, Result},
    llm::client::Llm,
};

pub mod client;
pub mod ollama;
pub mod types;

pub fn create_llm_client(config: &LlmConfig, prompt_cfg: &PromptConfig) -> Result<Arc<dyn Llm>> {
    match config.provider {
        LLMProvider::OpenAI => Err(Error::llm("OpenAI".to_string())),
        LLMProvider::Anthropic => Err(Error::llm("Anthropic".to_string())),
        LLMProvider::Ollama => {
            let llm = ollama::OllamaOpenAiCompat::new(config.clone(), prompt_cfg.clone())?;
            Ok(Arc::new(llm))
        }
    }
}

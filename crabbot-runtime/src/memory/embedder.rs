use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use crate::error::Result;

#[async_trait]
pub trait Embedder: Send + Sync + std::fmt::Debug {
    async fn embed(&self, input: &str) -> Result<Vec<f32>>;
    fn dim(&self) -> usize;
}

#[derive(Debug, Clone)]
pub struct OllamaOpenAiCompatEmbedder {
    base_url: String, // e.g. http://127.0.0.1:11434/v1
    model: String,
    http: Client,
    dim: usize,
}

impl OllamaOpenAiCompatEmbedder {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, dim: usize) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| crate::error::Error::other(e.to_string()))?;
        Ok(Self {
            base_url: base_url.into(),
            model: model.into(),
            http,
            dim,
        })
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingsItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingsItem {
    embedding: Vec<f32>,
}

#[async_trait]
impl Embedder for OllamaOpenAiCompatEmbedder {
    async fn embed(&self, input: &str) -> Result<Vec<f32>> {
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "input": input,
        });

        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::error::Error::other(e.to_string()))?;

        let text = resp
            .error_for_status()
            .map_err(|e| crate::error::Error::other(e.to_string()))?
            .text()
            .await
            .unwrap_or_default();

        let parsed: EmbeddingsResponse =
            serde_json::from_str(&text).map_err(|e| crate::error::Error::other(e.to_string()))?;

        let mut emb = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| crate::error::Error::other("embeddings response had no data"))?
            .embedding;

        if emb.len() != self.dim {
            return Err(crate::error::Error::other(format!(
                "embed dim mismatch: got {}, expected {}",
                emb.len(),
                self.dim
            )));
        }

        l2_normalize_in_place(&mut emb);
        Ok(emb)
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

fn l2_normalize_in_place(v: &mut [f32]) {
    let mut sum = 0.0f32;
    for &x in v.iter() {
        sum += x * x;
    }
    let norm = sum.sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

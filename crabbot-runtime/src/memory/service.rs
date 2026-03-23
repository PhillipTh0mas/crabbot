use std::path::{Path, PathBuf};
use std::sync::Arc;

use crabbot_shared::api::transcript::TranscriptEvent;
use tokio::fs;
use tokio::sync::Mutex;

use crate::config::{LLMProvider, MemoryConfig, MemoryKind};
use crate::error::Result;
use crate::memory::embedder::OllamaOpenAiCompatEmbedder;
use crate::memory::{
    chunking::chunk_text,
    embedder::Embedder,
    index::MemoryIndex,
    paths::MemoryPaths,
    store::MemoryStore,
    types::{MemorySearchHit, MemorySearchQuery, SearchFilters},
};

#[derive(Debug)]
struct MemoryInner {
    cfg: MemoryConfig,
    store: MemoryStore,
    index: MemoryIndex,
    embedder: Arc<dyn Embedder>,
}

#[derive(Debug, Clone)]
pub struct MemoryService {
    inner: Arc<Mutex<MemoryInner>>,
}

fn embedder_from_config(cfg: &MemoryConfig) -> Result<Arc<dyn Embedder>> {
    match cfg.embed_model_provider {
        LLMProvider::Ollama => {
            let embedder = Arc::new(OllamaOpenAiCompatEmbedder::new(
                &cfg.model_provider_base_url, // or cfg.memory.embed_base_url
                &cfg.embed_model,             // pick your embed model name
                cfg.embed_dim,
            )?);
            Ok(embedder)
        }
        _ => Err(crate::error::Error::other(format!(
            "Unsupported embedder provider: {:?}",
            cfg.embed_model_provider
        ))),
    }
}

impl MemoryService {
    pub async fn open(memory_dir: PathBuf, cfg: MemoryConfig) -> Result<Self> {
        let embedder = embedder_from_config(&cfg)?;

        let paths = MemoryPaths::new(memory_dir);
        let store = MemoryStore::open(paths, cfg.max_get_chars).await?;

        let sqlite_path = store.paths().sqlite_path.clone();
        let dim = cfg.embed_dim;
        let index = MemoryIndex::open(&sqlite_path, dim).await?;

        let inner = MemoryInner {
            cfg,
            store,
            index,
            embedder,
        };

        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    pub async fn paths(&self) -> MemoryPaths {
        let g = self.inner.lock().await;
        g.store.paths().clone()
    }

    pub async fn get_short_term(&self) -> Result<String> {
        let g = self.inner.lock().await;
        g.store.read_long_term().await
    }

    pub async fn update_short_term(&self, text: &str) -> Result<()> {
        let g = self.inner.lock().await;
        let _ = g.store.replace_long_term(text).await?;
        Ok(())
    }

    pub async fn get_daily(&self, ymd: &str) -> Result<String> {
        let g = self.inner.lock().await;
        g.store.read_daily(ymd).await
    }

    pub async fn write_daily_append(&self, ymd: &str, text: &str) -> Result<()> {
        let (path, cfg, embedder, index) = {
            let g = self.inner.lock().await;
            let path = g.store.append_daily(ymd, text).await?;
            (path, g.cfg.clone(), g.embedder.clone(), g.index.clone())
        };

        reindex_path(
            &cfg,
            embedder,
            index,
            &path,
            Some(ymd.to_string()),
            MemoryKind::Daily,
        )
        .await
    }

    pub async fn update_daily_replace(&self, ymd: &str, text: &str) -> Result<()> {
        let (path, cfg, embedder, index) = {
            let g = self.inner.lock().await;
            let path = g.store.replace_daily(ymd, text).await?;
            (path, g.cfg.clone(), g.embedder.clone(), g.index.clone())
        };

        if let Err(e) = reindex_path(
            &cfg,
            embedder,
            index,
            &path,
            Some(ymd.to_string()),
            MemoryKind::Daily,
        )
        .await
        {
            tracing::warn!("Failed to reindex daily memory for {ymd} (file saved ok): {e}");
        }
        Ok(())
    }

    pub async fn write_short_term_replace(&self, text: &str) -> Result<()> {
        let (path, cfg, embedder, index) = {
            let g = self.inner.lock().await;
            let path = g.store.replace_long_term(text).await?;
            (path, g.cfg.clone(), g.embedder.clone(), g.index.clone())
        };

        if let Err(e) = reindex_path(&cfg, embedder, index, &path, None, MemoryKind::LongTerm).await
        {
            tracing::warn!("Failed to reindex short-term memory (file saved ok): {e}");
        }
        Ok(())
    }

    pub async fn search_index(&self, q: MemorySearchQuery) -> Result<Vec<MemorySearchHit>> {
        let (cfg, embedder, index, kind_str, date_from, date_to) = {
            let g = self.inner.lock().await;
            (
                g.cfg.clone(),
                g.embedder.clone(),
                g.index.clone(),
                q.kind.map(|k| k.as_str().to_string()),
                q.date_from.clone(),
                q.date_to.clone(),
            )
        };

        let qemb = embedder.embed(&q.query).await?;
        let top_k = if q.top_k == 0 {
            cfg.default_top_k
        } else {
            q.top_k
        };

        let filters = SearchFilters {
            kind: kind_str,
            date_from,
            date_to,
        };

        index.search(&qemb, top_k, filters).await
    }

    pub async fn save_to_index(&self, text: &str, source: &str) -> Result<()> {
        let (cfg, embedder, index) = {
            let g = self.inner.lock().await;
            (g.cfg.clone(), g.embedder.clone(), g.index.clone())
        };

        // Reuse existing chunking logic; it expects a "path" string.
        let chunks = chunk_text(&cfg, MemoryKind::LongTerm, None, source, &text);

        if chunks.is_empty() {
            return Ok(());
        }

        let mut embeddings = Vec::with_capacity(chunks.len());
        for c in &chunks {
            embeddings.push(embedder.embed(&c.text).await?);
        }

        index.upsert_chunks(&chunks, &embeddings).await?;
        Ok(())
    }

    pub async fn reindex_all(&self) -> Result<()> {
        let (cfg, embedder, index, paths) = {
            let g = self.inner.lock().await;
            (
                g.cfg.clone(),
                g.embedder.clone(),
                g.index.clone(),
                g.store.paths().clone(),
            )
        };

        reindex_path(
            &cfg,
            embedder.clone(),
            index.clone(),
            &paths.memory_md,
            None,
            MemoryKind::LongTerm,
        )
        .await?;

        let mut rd = fs::read_dir(&paths.daily_dir)
            .await
            .map_err(|e| crate::error::Error::io(format!("read_dir failed: {e}")))?;

        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| crate::error::Error::io(format!("read_dir next_entry failed: {e}")))?
        {
            let p = entry.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let ymd = match p.file_stem().and_then(|x| x.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            reindex_path(
                &cfg,
                embedder.clone(),
                index.clone(),
                &p,
                Some(ymd),
                MemoryKind::Daily,
            )
            .await?;
        }

        Ok(())
    }

    pub async fn build_prompt_events(
        &self,
        include_long_term: bool,
        include_daily_days: usize, // e.g. 2 for today+yesterday
        max_chars: usize,
    ) -> Result<Vec<TranscriptEvent>> {
        let mut out = Vec::new();
        let mut buf = String::new();

        if include_long_term {
            let s = self.get_short_term().await.unwrap_or_default();
            if !s.trim().is_empty() {
                buf.push_str("Long-term memory:\n");
                buf.push_str(&truncate_chars(&s, max_chars));
                buf.push_str("\n\n");
            }
        }

        if include_daily_days > 0 {
            buf.push_str("Daily memory:\n");
            for ymd in recent_local_days(include_daily_days) {
                let s = self.get_daily(&ymd).await.unwrap_or_default();
                if s.trim().is_empty() {
                    continue;
                }
                buf.push_str(&format!("- {ymd}\n"));
                buf.push_str(&truncate_chars(&s, max_chars));
                buf.push_str("\n\n");
            }
        }

        if !buf.trim().is_empty() {
            out.push(TranscriptEvent::custom_message("system", buf));
        }

        Ok(out)
    }
}

async fn reindex_path(
    cfg: &MemoryConfig,
    embedder: Arc<dyn Embedder>,
    index: MemoryIndex,
    path: &Path,
    date: Option<String>,
    kind: MemoryKind,
) -> Result<()> {
    let content = fs::read_to_string(path).await.unwrap_or_default();
    let path_s = path.to_string_lossy().to_string();

    let chunks = chunk_text(cfg, kind, date.clone(), &path_s, &content);

    if chunks.is_empty() {
        index.delete_by_path(&path_s).await?;
        return Ok(());
    }

    let mut embeddings = Vec::with_capacity(chunks.len());
    for c in &chunks {
        embeddings.push(embedder.embed(&c.text).await?);
    }

    index.delete_by_path(&path_s).await?;
    index.upsert_chunks(&chunks, &embeddings).await?;

    Ok(())
}

fn recent_local_days(n: usize) -> Vec<String> {
    // n=2 => [yesterday, today]
    // You already have local_day_string(). Implement yesterday helper if missing.
    // Keep deterministic order from oldest to newest.
    let mut out = Vec::new();
    if n >= 2 {
        out.push(crate::time::local_day_string_yesterday());
    }
    out.push(crate::time::local_day_string());
    out.truncate(n);
    out
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("…");
    out
}

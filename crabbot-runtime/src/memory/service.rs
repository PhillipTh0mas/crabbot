use std::path::{Path, PathBuf};
use std::sync::Arc;

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

    pub async fn get_long_term(&self) -> Result<String> {
        let g = self.inner.lock().await;
        g.store.read_long_term().await
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

    pub async fn write_long_term_replace(&self, text: &str) -> Result<()> {
        let (path, cfg, embedder, index) = {
            let g = self.inner.lock().await;
            let path = g.store.replace_long_term(text).await?;
            (path, g.cfg.clone(), g.embedder.clone(), g.index.clone())
        };

        reindex_path(&cfg, embedder, index, &path, None, MemoryKind::LongTerm).await
    }

    pub async fn search(&self, q: MemorySearchQuery) -> Result<Vec<MemorySearchHit>> {
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

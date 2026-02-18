use std::path::PathBuf;

use tokio::fs;

use crate::{
    error::Result,
    memory::{paths::MemoryPaths, types::trunc_chars},
};

#[derive(Debug)]
pub(crate) struct MemoryStore {
    paths: MemoryPaths,
    max_get_chars: usize,
}

impl MemoryStore {
    pub async fn open(paths: MemoryPaths, max_get_chars: usize) -> Result<Self> {
        fs::create_dir_all(&paths.base)
            .await
            .map_err(|e| crate::error::Error::io(format!("create_dir_all failed: {e}")))?;
        fs::create_dir_all(&paths.daily_dir)
            .await
            .map_err(|e| crate::error::Error::io(format!("create_dir_all failed: {e}")))?;
        fs::create_dir_all(&paths.index_dir)
            .await
            .map_err(|e| crate::error::Error::io(format!("create_dir_all failed: {e}")))?;

        if fs::metadata(&paths.memory_md).await.is_err() {
            fs::write(&paths.memory_md, b"")
                .await
                .map_err(|e| crate::error::Error::io(format!("write MEMORY.md failed: {e}")))?;
        }

        Ok(Self {
            paths,
            max_get_chars,
        })
    }

    pub fn paths(&self) -> &MemoryPaths {
        &self.paths
    }

    pub async fn read_long_term(&self) -> Result<String> {
        let s = fs::read_to_string(&self.paths.memory_md)
            .await
            .unwrap_or_default();
        Ok(trunc_chars(&s, self.max_get_chars))
    }

    pub async fn read_daily(&self, ymd: &str) -> Result<String> {
        let p = self.paths.daily_file(ymd);
        let s = fs::read_to_string(p).await.unwrap_or_default();
        Ok(trunc_chars(&s, self.max_get_chars))
    }

    pub async fn append_daily(&self, ymd: &str, text: &str) -> Result<PathBuf> {
        let p = self.paths.daily_file(ymd);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::error::Error::io(format!("create_dir_all failed: {e}")))?;
        }

        let mut existing = fs::read_to_string(&p).await.unwrap_or_default();
        if !existing.ends_with('\n') && !existing.is_empty() {
            existing.push('\n');
        }
        existing.push_str(text.trim());
        existing.push('\n');

        fs::write(&p, existing)
            .await
            .map_err(|e| crate::error::Error::io(format!("write daily failed: {e}")))?;

        Ok(p)
    }

    pub async fn replace_long_term(&self, text: &str) -> Result<PathBuf> {
        fs::write(&self.paths.memory_md, text)
            .await
            .map_err(|e| crate::error::Error::io(format!("write MEMORY.md failed: {e}")))?;
        Ok(self.paths.memory_md.clone())
    }
}

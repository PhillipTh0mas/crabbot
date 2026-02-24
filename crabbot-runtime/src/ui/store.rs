use crabbot_shared::api::ui_html::UiHtmlUpdate;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::fs;
use tokio::sync::{RwLock, broadcast};

use crate::error::{Error, Result};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum UiHtmlArgs {
    Load,
    Save {
        html: String,
        #[serde(default = "default_true")]
        overwrite: bool,
    },
    Delete,
}

#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum UiHtmlOut {
    Load { exists: bool, html: String },
    Save { bytes_written: usize },
    Delete { deleted: bool },
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn truncate_bytes(mut s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n<!-- truncated -->\n");
    s
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Clone, Debug)]
pub struct UiHtmlStore {
    inner: Arc<UiHtmlStoreInner>,
}

#[derive(Debug)]
struct UiHtmlStoreInner {
    ui_dir: PathBuf,
    max_html_bytes: usize,
    // per (session_key, name)
    bus: RwLock<HashMap<String, broadcast::Sender<UiHtmlUpdate>>>,
}

impl UiHtmlStore {
    pub fn new(ui_dir: PathBuf) -> Self {
        Self::new_with_limits(ui_dir, 512 * 1024, 128)
    }

    pub fn new_with_limits(ui_dir: PathBuf, max_html_bytes: usize, _chan_cap: usize) -> Self {
        Self {
            inner: Arc::new(UiHtmlStoreInner {
                ui_dir,
                max_html_bytes,
                bus: RwLock::new(HashMap::new()),
            }),
        }
    }

    fn html_path(&self, session_key: &str) -> PathBuf {
        let session = sanitize(session_key);
        self.inner.ui_dir.join(session).join("content.html")
    }

    async fn sender(&self, session_key: &str) -> broadcast::Sender<UiHtmlUpdate> {
        let mut map = self.inner.bus.write().await;
        let key = session_key.to_string();
        map.entry(key)
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(128);
                tx
            })
            .clone()
    }

    pub async fn subscribe(&self, session_key: &str) -> broadcast::Receiver<UiHtmlUpdate> {
        self.sender(session_key).await.subscribe()
    }

    pub async fn load(&self, session_key: &str) -> Result<(bool, String)> {
        let path = self.html_path(session_key);
        match fs::read_to_string(&path).await {
            Ok(html) => Ok((true, truncate_bytes(html, self.inner.max_html_bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((false, String::new())),
            Err(e) => Err(Error::io(format!("ui_html load failed: {e}"))),
        }
    }

    pub async fn save(&self, session_key: &str, html: String, overwrite: bool) -> Result<usize> {
        let path = self.html_path(session_key);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::io(format!("create session dir failed: {e}")))?;
        }

        if !overwrite && fs::metadata(&path).await.is_ok() {
            return Err(Error::bad_request(format!(
                "view '{session_key}' already exists (overwrite=false)"
            )));
        }

        let html = truncate_bytes(html, self.inner.max_html_bytes);

        fs::write(&path, html.as_bytes())
            .await
            .map_err(|e| Error::io(format!("ui_html write failed: {e}")))?;

        let bytes = html.len();

        let tx = self.sender(session_key).await;
        let _ = tx.send(UiHtmlUpdate::Saved {
            ts_ms: now_ms(),
            bytes,
        });

        Ok(bytes)
    }

    pub async fn delete(&self, session_key: &str) -> Result<bool> {
        let path = self.html_path(session_key);

        match fs::remove_file(&path).await {
            Ok(()) => {
                let tx = self.sender(session_key).await;
                let _ = tx.send(UiHtmlUpdate::Deleted { ts_ms: now_ms() });
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::io(format!("ui_html delete failed: {e}"))),
        }
    }

    // convenience if you want the tool to keep its current enum output shape
    pub async fn call(&self, session_key: &str, args: UiHtmlArgs) -> Result<UiHtmlOut> {
        match args {
            UiHtmlArgs::Load => {
                let (exists, html) = self.load(session_key).await?;
                Ok(UiHtmlOut::Load { exists, html })
            }
            UiHtmlArgs::Save { html, overwrite } => {
                let bytes_written = self.save(session_key, html, overwrite).await?;
                Ok(UiHtmlOut::Save { bytes_written })
            }
            UiHtmlArgs::Delete => {
                let deleted = self.delete(session_key).await?;
                Ok(UiHtmlOut::Delete { deleted })
            }
        }
    }
}

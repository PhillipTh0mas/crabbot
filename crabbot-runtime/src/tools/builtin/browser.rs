use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use base64::Engine as _;
use chromiumoxide::{
    Page,
    browser::{Browser, BrowserConfig},
    cdp::browser_protocol::page::CaptureScreenshotFormat,
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use tokio::time::{Duration, sleep};
use url::Url;

use crate::{
    error::{Error, Result},
    tools::tool::{Tool, ToolCall, ToolResult, ToolSpec},
};

//
// ==============================
// CONFIGURE YOUR PINNED BUILD
// ==============================
// Use a fixed revision for reproducibility.
// Example below is placeholder — replace with real artifact.
//
struct ChromiumArtifact {
    url: &'static str,
    sha256: &'static str,
    exe_relpath: &'static str,
}

#[cfg(target_os = "linux")]
const CHROMIUM_ARTIFACT: ChromiumArtifact = ChromiumArtifact {
    url: "https://your-mirror/chromium-linux.zip",
    sha256: "0e07f6576682189e50e54f9b89e0ee1d3f3f94a639a6a39aea66e68132c0aaf9",
    exe_relpath: "chrome-linux/chrome",
};

#[cfg(target_os = "macos")]
const CHROMIUM_ARTIFACT: ChromiumArtifact = ChromiumArtifact {
    url: "https://your-mirror/chromium-mac.zip",
    sha256: "8c9dc1a7cbcfce3a9c1e4f4d2d80a3c6329a2b805d6e7ca3a0655e768f8b510c",
    exe_relpath: "Chromium.app/Contents/MacOS/Chromium",
};

#[cfg(target_os = "windows")]
const CHROMIUM_ARTIFACT: ChromiumArtifact = ChromiumArtifact {
    url: "https://your-mirror/chromium-win.zip",
    sha256: "b02e37bdbb02a1b7f9a0efc9a5a9a3d0b3d3f4b12f4d2c5c4a8a5e2f3c7d9b1e",
    exe_relpath: "chrome-win/chrome.exe",
};

//
// ==============================
// TOOL STRUCT
// ==============================
//

#[derive(Debug, Clone)]
pub struct BrowserTool {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    browser: Mutex<Option<Browser>>,
    pages: Mutex<HashMap<String, Page>>,
    browser_folder: PathBuf,
}

impl BrowserTool {
    pub fn new(browser_folder: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                browser: Mutex::new(None),
                pages: Mutex::new(HashMap::new()),
                browser_folder,
            }),
        }
    }

    async fn ensure_browser(&self) -> Result<Browser> {
        if let Some(b) = self.inner.browser.lock().clone() {
            return Ok(b);
        }

        let exe = ensure_chromium(&self.inner.browser_folder).await?;

        let cfg = BrowserConfig::builder()
            .chrome_executable(exe.to_string_lossy().to_string())
            .headless(true)
            .no_sandbox(true)
            .build()
            .map_err(Error::other)?;

        let (browser, mut handler) = Browser::launch(cfg).await.map_err(Error::other)?;

        tokio::spawn(async move {
            let _ = handler.run().await;
        });

        *self.inner.browser.lock() = Some(browser.clone());
        Ok(browser)
    }

    async fn page_for(&self, session: &str) -> Result<Page> {
        if let Some(p) = self.inner.pages.lock().get(session).cloned() {
            return Ok(p);
        }

        let browser = self.ensure_browser().await?;
        let page = browser
            .new_page("about:blank")
            .await
            .map_err(Error::other)?;

        self.inner
            .pages
            .lock()
            .insert(session.to_string(), page.clone());
        Ok(page)
    }
}

//
// ==============================
// CHROMIUM INSTALLATION
// ==============================
//

async fn ensure_chromium(cache_root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(cache_root)?;
    let install_root = cache_root.join("chromium");
    let exe_path = install_root.join(CHROMIUM_ARTIFACT.exe_relpath);

    if exe_path.exists() {
        return Ok(exe_path);
    }

    let archive_path = cache_root.join("chromium.zip");

    let bytes = reqwest::get(CHROMIUM_ARTIFACT.url)
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    // verify checksum
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = format!("{:x}", hasher.finalize());

    if hash != CHROMIUM_ARTIFACT.sha256 {
        Error::config("chromium checksum mismatch");
    }

    tokio::fs::write(&archive_path, &bytes).await?;

    extract_zip(&archive_path, &install_root)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&exe_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&exe_path, perms)?;
    }

    Ok(exe_path)
}

fn extract_zip(archive: &Path, dst: &Path) -> Result<()> {
    let data = fs::read(archive)?;
    let reader = Cursor::new(data);
    let mut zip = zip::ZipArchive::new(reader)?;

    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let out_path = dst.join(file.mangled_name());

        if file.name().ends_with('/') {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut outfile = fs::File::create(&out_path)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }

    Ok(())
}

//
// ==============================
// TOOL ARGUMENTS
// ==============================
//

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Args {
    Open { url: String },
    ExtractText,
    Find { needle: String },
    Click { selector: String },
    Type { selector: String, text: String },
    Screenshot,
    CurrentUrl,
    ResetSession,
}

//
// ==============================
// TOOL IMPLEMENTATION
// ==============================
//

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser".into(),
            description: "Headless Chromium browser tool".into(),
            parameters: json!({ "type": "object" }),
        }
    }

    async fn call(&self, call: ToolCall, session_key: &str) -> Result<ToolResult> {
        let args: Args = serde_json::from_value(call.args)?;

        match args {
            Args::Open { url } => {
                Url::parse(&url)?;
                let page = self.page_for(session_key).await?;
                page.goto(&url).await?;
                sleep(Duration::from_millis(400)).await;
                Ok(ToolResult::ok(call.id, call.name, json!({"status":"ok"})))
            }

            Args::ExtractText => {
                let page = self.page_for(session_key).await?;
                let js = r#"document.body.innerText"#;
                let text: String = page.evaluate(js).await?.value().map_err(Error::other)?;
                Ok(ToolResult::ok(call.id, call.name, json!({ "text": text })))
            }

            Args::Find { needle } => {
                let page = self.page_for(session_key).await?;
                let js = r#"document.body.innerText"#;
                let text: String = page.evaluate(js).await?.value().map_err(Error::other)?;
                let hits: Vec<_> = text
                    .lines()
                    .filter(|l| l.contains(&needle))
                    .take(20)
                    .collect();
                Ok(ToolResult::ok(call.id, call.name, json!({ "hits": hits })))
            }

            Args::Click { selector } => {
                let page = self.page_for(session_key).await?;
                let js = format!(
                    "document.querySelector({}).click()",
                    serde_json::to_string(&selector)?
                );
                page.evaluate(&js).await?;
                Ok(ToolResult::ok(call.id, call.name, json!({"status":"ok"})))
            }

            Args::Type { selector, text } => {
                let page = self.page_for(session_key).await?;
                let js = format!(
                    r#"
                    let el = document.querySelector({});
                    el.value = {};
                    el.dispatchEvent(new Event("input", {{ bubbles: true }}));
                    "#,
                    serde_json::to_string(&selector)?,
                    serde_json::to_string(&text)?
                );
                page.evaluate(&js).await?;
                Ok(ToolResult::ok(call.id, call.name, json!({"status":"ok"})))
            }

            Args::Screenshot => {
                let page = self.page_for(session_key).await?;
                let png = page.screenshot(CaptureScreenshotFormat::Png).await?;
                let b64 = base64::engine::general_purpose::STANDARD.encode(png);
                Ok(ToolResult::ok(
                    call.id,
                    call.name,
                    json!({"png_base64": b64}),
                ))
            }

            Args::CurrentUrl => {
                let page = self.page_for(session_key).await?;
                let url = page.url().await?;
                Ok(ToolResult::ok(call.id, call.name, json!({ "url": url })))
            }

            Args::ResetSession => {
                self.inner.pages.lock().remove(session_key);
                Ok(ToolResult::ok(call.id, call.name, json!({"status":"ok"})))
            }
        }
    }
}

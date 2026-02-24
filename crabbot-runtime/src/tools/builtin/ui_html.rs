use async_trait::async_trait;
use serde_json::json;

use crate::error::{Error, Result};
use crate::tools::tool::{Tool, ToolCall, ToolResult, ToolSpec};
use crate::ui::store::{UiHtmlArgs, UiHtmlStore}; // adjust module path

#[derive(Debug)]
pub struct UiHtmlTool {
    store: UiHtmlStore,
}

impl UiHtmlTool {
    pub fn new(store: UiHtmlStore) -> Self {
        Self { store }
    }
}

const MAX_HTML_BYTES: usize = 250_000;

/// Prepare HTML for innerHTML injection:
/// - If a full HTML document is provided, keep only <body>...</body> inner contents.
/// - Enforce a size limit after stripping.
/// - Lightweight sanity check (does NOT block <script> or on* handlers).
fn prepare_inner_html(mut html: String) -> Result<String> {
    html = strip_to_body_inner_html(&html);

    let n = html.as_bytes().len();
    if n > MAX_HTML_BYTES {
        return Err(Error::bad_request(format!(
            "ui html too large after stripping ({} bytes > {} bytes)",
            n, MAX_HTML_BYTES
        )));
    }

    let lt = html.chars().filter(|&c| c == '<').count();
    let gt = html.chars().filter(|&c| c == '>').count();
    if lt != gt {
        return Err(Error::bad_request(
            "ui html appears malformed (unbalanced '<' and '>')",
        ));
    }

    Ok(html)
}

/// If <body ...>...</body> exists, return the inner HTML.
/// Otherwise, tries to remove <head>...</head> and outer <html>...</html> wrapper,
/// leaving something suitable for innerHTML.
fn strip_to_body_inner_html(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }

    // Use ASCII-lowercase for stable byte indexing.
    let lower = s.to_ascii_lowercase();

    // Prefer body extraction if present.
    if let Some(body_open) = lower.find("<body") {
        // Find end of opening <body ...>
        if let Some(gt_rel) = lower[body_open..].find('>') {
            let body_start = body_open + gt_rel + 1;

            // Find closing </body>
            if let Some(body_close) = lower[body_start..].find("</body") {
                let body_end = body_start + body_close;
                return s[body_start..body_end].trim().to_string();
            }
        }
        // If there's a <body but it's malformed, fall through to heuristic stripping.
    }

    // Remove head block if present.
    let mut out = s.to_string();
    out = remove_tag_block_case_insensitive(&out, "head");

    // Remove outer <html> wrapper if present, keeping its inner content.
    out = unwrap_outer_tag_case_insensitive(&out, "html");

    // Remove <!doctype ...> if present at the start.
    out = remove_doctype_prefix(&out);

    out.trim().to_string()
}

fn remove_doctype_prefix(input: &str) -> String {
    let s = input.trim_start();
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("<!doctype") {
        if let Some(end) = lower.find('>') {
            return s[end + 1..].trim_start().to_string();
        }
    }
    input.to_string()
}

/// Removes a full tag block like <head ...> ... </head> (first occurrence), case-insensitive.
/// If malformed (missing closing), returns input unchanged.
fn remove_tag_block_case_insensitive(input: &str, tag: &str) -> String {
    let s = input;
    let lower = s.to_ascii_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}");

    let open = match lower.find(&open_pat) {
        Some(i) => i,
        None => return s.to_string(),
    };

    let open_gt = match lower[open..].find('>') {
        Some(r) => open + r + 1,
        None => return s.to_string(),
    };

    let close = match lower[open_gt..].find(&close_pat) {
        Some(r) => open_gt + r,
        None => return s.to_string(),
    };

    let close_gt = match lower[close..].find('>') {
        Some(r) => close + r + 1,
        None => return s.to_string(),
    };

    let mut out = String::with_capacity(s.len().saturating_sub(close_gt - open));
    out.push_str(&s[..open]);
    out.push_str(&s[close_gt..]);
    out
}

/// If the string contains an outer <html ...> ... </html>, unwrap to just its inner content.
/// If malformed (missing closing), returns input unchanged.
fn unwrap_outer_tag_case_insensitive(input: &str, tag: &str) -> String {
    let s = input.trim();
    let lower = s.to_ascii_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}");

    let open = match lower.find(&open_pat) {
        Some(i) => i,
        None => return input.to_string(),
    };

    let open_gt = match lower[open..].find('>') {
        Some(r) => open + r + 1,
        None => return input.to_string(),
    };

    let close = match lower[open_gt..].find(&close_pat) {
        Some(r) => open_gt + r,
        None => return input.to_string(),
    };

    s[open_gt..close].trim().to_string()
}

#[async_trait]
impl Tool for UiHtmlTool {
    fn name(&self) -> &str {
        "render_user_ui_html"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "Use this tool to render your response as HTML in the user-facing web UI (innerHTML), instead of replying with plain text. Save/load/delete a per-session HTML snippet. Generate Tailwind-styled HTML (divs, layouts, cards, tables, dashboards, iframes, live views). If you accidentally generate a full HTML document, only the <body> content will be used."
                .to_string(),
            parameters: json!({
                "type": "object",
                "oneOf": [
                    {
                        "type": "object",
                        "required": ["op"],
                        "properties": {
                            "op": { "const": "load" }
                        }
                    },
                    {
                        "type": "object",
                        "required": ["op", "html"],
                        "properties": {
                            "op": { "const": "save" },
                            "html": { "type": "string" },
                            "overwrite": { "type": "boolean", "default": true }
                        }
                    },
                    {
                        "type": "object",
                        "required": ["op"],
                        "properties": {
                            "op": { "const": "delete" }
                        }
                    }
                ]
            }),
        }
    }

    async fn call(&self, call: ToolCall, session_key: &str) -> Result<ToolResult> {
        let mut args: UiHtmlArgs = serde_json::from_value(call.args)
            .map_err(|e| Error::bad_request(format!("ui html tool: invalid args: {e}")))?;

        // Normalize/validate only for save.
        // Adjust match arms if your UiHtmlArgs shape differs.
        args = match args {
            UiHtmlArgs::Save { html, overwrite } => {
                let html = prepare_inner_html(html)?;
                UiHtmlArgs::Save { html, overwrite }
            }
            other => other,
        };

        let payload = self.store.call(session_key, args).await?;

        Ok(ToolResult::ok(
            call.id,
            call.name,
            serde_json::to_value(payload)
                .map_err(|e| Error::tool(format!("serialize failed: {e}")))?,
        ))
    }
}

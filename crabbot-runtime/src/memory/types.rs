use serde::Serialize;

use crate::config::MemoryKind;

#[derive(Debug, Clone, Default)]
pub struct FlushResult {
    pub daily: Vec<String>,
    pub long_term: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySearchHit {
    pub chunk_id: String,
    pub kind: String,
    pub date: Option<String>,
    pub path: String,
    pub distance: f64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct MemorySearchQuery {
    pub query: String,
    pub top_k: usize,
    pub kind: Option<MemoryKind>,
    pub date_from: Option<String>, // "YYYY-MM-DD"
    pub date_to: Option<String>,   // "YYYY-MM-DD"
}

#[derive(Debug, Clone)]
pub(crate) struct Chunk {
    pub chunk_id: String,
    pub kind: MemoryKind,
    pub date: Option<String>,
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Debug, Default, Clone)]
pub struct SearchFilters {
    pub kind: Option<String>,      // "daily"|"long_term"
    pub date_from: Option<String>, // inclusive
    pub date_to: Option<String>,   // inclusive
}

impl SearchFilters {
    pub fn matches(&self, kind: &str, date: &Option<String>) -> bool {
        if let Some(k) = &self.kind {
            if k != kind {
                return false;
            }
        }
        if self.date_from.is_some() || self.date_to.is_some() {
            let Some(d) = date else { return false };
            if let Some(from) = &self.date_from {
                if d < from {
                    return false;
                }
            }
            if let Some(to) = &self.date_to {
                if d > to {
                    return false;
                }
            }
        }
        true
    }
}

pub(crate) fn trunc_chars(s: &str, max_chars: usize) -> String {
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

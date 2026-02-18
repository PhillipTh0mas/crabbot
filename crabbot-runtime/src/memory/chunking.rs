use sha2::{Digest, Sha256};

use crate::config::{MemoryConfig, MemoryKind};
use crate::memory::types::Chunk;

pub(crate) fn chunk_text(
    cfg: &MemoryConfig,
    kind: MemoryKind,
    date: Option<String>,
    path: &str,
    content: &str,
) -> Vec<Chunk> {
    let s = content.trim();
    if s.is_empty() {
        return vec![];
    }

    let maxc = cfg.chunk_max_chars.max(200);
    let overlap = cfg.chunk_overlap_chars.min(maxc.saturating_sub(1));

    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();

    while i < bytes.len() {
        let mut j = (i + maxc).min(bytes.len());

        while j > i && !s.is_char_boundary(j) {
            j -= 1;
        }
        if j == i {
            j = (i + 1).min(bytes.len());
            while j < bytes.len() && !s.is_char_boundary(j) {
                j += 1;
            }
        }

        let piece = s[i..j].trim();
        if !piece.is_empty() {
            let chunk_id = make_chunk_id(path, &date, i, j, piece);
            out.push(Chunk {
                chunk_id,
                kind,
                date: date.clone(),
                path: path.to_string(),
                start: i,
                end: j,
                text: piece.to_string(),
            });
        }

        if j == bytes.len() {
            break;
        }

        let next_i = j.saturating_sub(overlap);
        i = if next_i > i { next_i } else { j };
    }

    out
}

fn make_chunk_id(
    path: &str,
    date: &Option<String>,
    start: usize,
    end: usize,
    text: &str,
) -> String {
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    h.update(b"|");
    if let Some(d) = date {
        h.update(d.as_bytes());
    }
    h.update(b"|");
    h.update(start.to_le_bytes());
    h.update(end.to_le_bytes());
    h.update(b"|");
    h.update(text.as_bytes());
    let digest = h.finalize();
    let chunk_id = digest.iter().map(|b| format!("{:02x}", b)).collect();
    chunk_id
}

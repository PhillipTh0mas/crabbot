use std::{path::PathBuf, usize};

use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
};

use crabbot_shared::api::transcript::TranscriptEvent;

#[derive(Debug, Clone)]
pub struct TranscriptStore {
    dir: PathBuf,
}

impl TranscriptStore {
    pub fn open(dir: PathBuf) -> crate::error::Result<Self> {
        if dir.as_os_str().is_empty() {
            return Err(crate::error::Error::io("transcript dir must not be empty"));
        }
        std::fs::create_dir_all(&dir).map_err(|e| {
            crate::error::Error::io(format!("failed to create transcript dir: {e}"))
        })?;
        Ok(Self { dir })
    }

    fn sanitize_session_id(session_id: &str) -> crate::error::Result<String> {
        // Keep filenames safe and predictable; reject empty or suspicious inputs.
        let s = session_id.trim();
        if s.is_empty() {
            return Err(crate::error::Error::bad_request(
                "session_id must not be empty",
            ));
        }
        if s.len() > 200 {
            return Err(crate::error::Error::bad_request("session_id too long"));
        }
        // Allow a conservative charset for file names.
        if !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(crate::error::Error::bad_request(
                "session_id contains invalid characters (allowed: [A-Za-z0-9-_.])",
            ));
        }
        if s == "." || s == ".." {
            return Err(crate::error::Error::bad_request("invalid session_id"));
        }
        Ok(s.to_string())
    }

    fn session_path(&self, session_id: &str) -> crate::error::Result<PathBuf> {
        let sid = Self::sanitize_session_id(session_id)?;
        Ok(self.dir.join(format!("{sid}.jsonl")))
    }

    pub async fn append(
        &self,
        session_id: &str,
        event: TranscriptEvent,
    ) -> crate::error::Result<()> {
        let path = self.session_path(session_id)?;

        // Ensure parent exists (in case dir was deleted after open()).
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::error::Error::io(format!("create_dir_all failed: {e}")))?;
        }

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| crate::error::Error::io(format!("open transcript file failed: {e}")))?;

        let mut line = serde_json::to_string(&event).map_err(|e| {
            crate::error::Error::io(format!("serialize TranscriptEvent failed: {e}"))
        })?;
        line.push('\n');

        file.write_all(line.as_bytes())
            .await
            .map_err(|e| crate::error::Error::io(format!("write transcript failed: {e}")))?;

        // Optional but useful for tailing: ensure durability for long-running sessions.
        file.flush()
            .await
            .map_err(|e| crate::error::Error::io(format!("flush transcript failed: {e}")))?;

        Ok(())
    }

    async fn get_file(&self, session_id: &str) -> crate::error::Result<fs::File> {
        let path = self.session_path(session_id)?;

        match fs::File::open(&path).await {
            Ok(f) => Ok(f),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(crate::error::Error::io("file not found"))
            }
            Err(e) => Err(crate::error::Error::io(format!(
                "open transcript file failed: {e}"
            ))),
        }
    }

    pub async fn read_tail(
        &self,
        session_id: &str,
        max_events: usize,
    ) -> crate::error::Result<Vec<TranscriptEvent>> {
        if max_events == 0 {
            return Ok(vec![]);
        }

        let Ok(file) = self.get_file(session_id).await else {
            return Ok(vec![]);
        };

        let mut reader = BufReader::new(file);
        let mut line = String::new();

        // Simple + robust approach: stream all lines, keep only last N (ring buffer).
        // If you expect huge transcripts, replace this with a backward-seek tail reader.
        let mut buf: std::collections::VecDeque<TranscriptEvent> =
            std::collections::VecDeque::with_capacity(max_events);

        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| crate::error::Error::io(format!("read transcript failed: {e}")))?;
            if n == 0 {
                break;
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<TranscriptEvent>(trimmed) {
                Ok(ev) => {
                    if buf.len() == max_events {
                        buf.pop_front();
                    }
                    buf.push_back(ev);
                }
                Err(_) => {
                    // Skip malformed lines to keep tailing resilient.
                    continue;
                }
            }
        }

        Ok(buf.into_iter().collect())
    }

    pub async fn count_events(&self, session_id: &str) -> crate::error::Result<usize> {
        let Ok(file) = self.get_file(session_id).await else {
            return Ok(0);
        };

        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let mut total: usize = 0;

        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| crate::error::Error::io(format!("read transcript failed: {e}")))?;
            if n == 0 {
                break;
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                continue;
            }

            total += 1;
        }

        Ok(total)
    }

    pub async fn read_head(
        &self,
        session_id: &str,
        max_events: usize,
    ) -> crate::error::Result<Vec<TranscriptEvent>> {
        if max_events == 0 {
            return Ok(vec![]);
        }

        let Ok(file) = self.get_file(session_id).await else {
            return Ok(vec![]);
        };

        let mut reader = BufReader::new(file);
        let mut out = Vec::with_capacity(max_events);
        let mut line = String::new();

        while out.len() < max_events {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| crate::error::Error::io(format!("read transcript failed: {e}")))?;
            if n == 0 {
                break;
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                continue;
            }

            if let Ok(ev) = serde_json::from_str::<TranscriptEvent>(trimmed) {
                out.push(ev);
            }
        }

        Ok(out)
    }

    pub async fn tail_size_after_head(
        &self,
        session_id: &str,
        head_num: usize,
    ) -> crate::error::Result<usize> {
        let total = self.count_events(session_id).await?;
        Ok(total.saturating_sub(head_num))
    }

    pub async fn rewrite_all(
        &self,
        session_id: &str,
        events: &[TranscriptEvent],
    ) -> crate::error::Result<()> {
        let path = self.session_path(session_id)?;
        let tmp_path = path.with_extension("jsonl.tmp");

        let tmp = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .await
            .map_err(|e| crate::error::Error::io(format!("open tmp transcript failed: {e}")))?;

        let mut writer = BufWriter::new(tmp);

        for ev in events {
            let mut line = serde_json::to_string(ev)
                .map_err(|e| crate::error::Error::io(format!("serialize failed: {e}")))?;
            line.push('\n');

            writer
                .write_all(line.as_bytes())
                .await
                .map_err(|e| crate::error::Error::io(format!("write failed: {e}")))?;
        }

        writer
            .flush()
            .await
            .map_err(|e| crate::error::Error::io(format!("flush failed: {e}")))?;

        fs::rename(&tmp_path, &path)
            .await
            .map_err(|e| crate::error::Error::io(format!("replace transcript failed: {e}")))?;

        Ok(())
    }

    pub async fn read_for_prompt(
        &self,
        session_id: &str,
        max_events: usize,
    ) -> crate::error::Result<Vec<TranscriptEvent>> {
        let events = self.read_tail(session_id, max_events).await?;

        // For prompt context, drop out-of-context events.
        // (CustomNote is explicitly out-of-context in your design.)
        let mut out = Vec::with_capacity(events.len());
        for ev in events {
            if matches!(ev, TranscriptEvent::CustomNote(_)) {
                continue;
            }
            out.push(ev);
        }

        // Optional: enforce tool-call/result ordering if you want to be strict.
        // For now, keep it simple.

        Ok(out)
    }

    pub async fn get_compact_events(
        &self,
        session_id: &str,
        max_events: usize,
    ) -> crate::error::Result<(Vec<TranscriptEvent>, i64)> {
        let total_events = self.count_events(session_id).await?;
        let head_cnt = usize::max(total_events / 2, max_events / 2);
        let compacted = self.read_head(session_id, head_cnt).await?;

        let mut max_time = 0i64;
        for event in &compacted {
            let t = event.ts_ms();
            if t > max_time {
                max_time = t;
            }
        }

        Ok((compacted, max_time))
    }

    pub async fn replace(
        &self,
        session_id: &str,
        head_num: usize,
        summary: TranscriptEvent,
    ) -> crate::error::Result<()> {
        let keep = self.tail_size_after_head(session_id, head_num).await?;

        let mut new_events = Vec::with_capacity(1 + keep);

        new_events.push(summary);

        if keep > 0 {
            let tail = self.read_tail(session_id, keep).await?;
            new_events.extend(tail);
        }

        self.rewrite_all(session_id, &new_events).await
    }

    pub async fn clear(&self, session_id: &str) -> crate::error::Result<()> {
        let path = self.session_path(session_id)?;

        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(crate::error::Error::io(format!(
                "remove transcript file failed: {e}"
            ))),
        }
    }

    // Optional: useful for "nuke everything" in tests.
    #[allow(dead_code)]
    pub async fn clear_all(&self) -> crate::error::Result<()> {
        let mut rd = fs::read_dir(&self.dir)
            .await
            .map_err(|e| crate::error::Error::io(format!("read_dir failed: {e}")))?;
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| crate::error::Error::io(format!("read_dir next_entry failed: {e}")))?
        {
            let p = entry.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                let _ = fs::remove_file(&p).await;
            }
        }
        Ok(())
    }
}

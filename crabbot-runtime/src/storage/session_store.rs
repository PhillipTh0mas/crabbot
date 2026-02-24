use std::{collections::HashMap, fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub session_key: String,
    pub session_id: String,
    pub model: Option<ModelOverride>,
    pub flags: SessionFlags,
    pub counters: SessionCounters,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelOverride {
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFlags {
    pub compaction_enabled: bool,
    pub daily_reset: bool,
}

impl Default for SessionFlags {
    fn default() -> Self {
        Self {
            compaction_enabled: true,
            daily_reset: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionCounters {
    // Activity
    #[serde(default)]
    pub last_activity_ts_ms: i64,
    #[serde(default)]
    pub last_activity_local_day: Option<String>, // "YYYY-MM-DD"
    #[serde(default)]
    pub approx_tokens: usize,

    // Session rotation
    #[serde(default)]
    pub last_reset_ts_ms: Option<i64>,
    #[serde(default)]
    pub reset_count: usize,

    // Compaction bookkeeping
    #[serde(default)]
    pub compaction_count: usize,
    #[serde(default)]
    pub last_compaction_ts_ms: Option<i64>,
    #[serde(default)]
    pub tokens_at_last_compaction: Option<usize>,

    // Pre-compaction memory flush bookkeeping
    // Idea: at most one memory flush per compaction "cycle".
    // `memory_flush_for_compaction_seq == compaction_count` means "already flushed for upcoming compaction".
    #[serde(default)]
    pub last_memory_flush_ts_ms: Option<i64>,
    #[serde(default)]
    pub memory_flush_for_compaction_seq: Option<usize>,

    #[serde(default)]
    pub last_reset_flush_ts_ms: Option<i64>,
    #[serde(default)]
    pub reset_flush_for_reset_seq: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionIndex {
    pub entries: HashMap<String, SessionEntry>,
}

#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    index: RwLock<SessionIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetReason {
    None,
    Daily,
    Idle,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionReason {
    TokenLimit,
    Forced,
}

#[derive(Debug, Clone)]
pub struct InitOutcome {
    pub entry: SessionEntry,
    pub did_create: bool,
    pub did_rotate_session_id: bool,
    pub reason: ResetReason,
    pub prev_session_id: Option<String>,
}

impl SessionStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        let index = if path.exists() {
            let data = fs::read_to_string(&path)?;
            serde_json::from_str(&data).map_err(|e| Error::io(e.to_string()))?
        } else {
            SessionIndex::default()
        };

        Ok(Self {
            path,
            index: RwLock::new(index),
        })
    }

    pub async fn list_session_keys(&self) -> Result<Vec<String>> {
        let idx = self.index.read().await;
        Ok(idx.entries.keys().cloned().collect())
    }

    pub async fn get(&self, session_key: &str) -> Result<SessionEntry> {
        let idx = self.index.read().await;
        if let Some(e) = idx.entries.get(session_key) {
            return Ok(e.clone());
        } else {
            Err(Error::not_found(format!(
                "session {} was not found",
                session_key
            )))
        }
    }

    pub async fn get_or_create(&self, session_key: &str) -> Result<SessionEntry> {
        {
            let idx = self.index.read().await;
            if let Some(e) = idx.entries.get(session_key) {
                return Ok(e.clone());
            }
        }

        let mut idx = self.index.write().await;
        if let Some(e) = idx.entries.get(session_key) {
            return Ok(e.clone());
        }

        let entry = SessionEntry {
            session_key: session_key.to_string(),
            session_id: new_session_id(),
            model: None,
            flags: SessionFlags::default(),
            counters: SessionCounters::default(),
        };

        idx.entries.insert(session_key.to_string(), entry.clone());
        drop(idx);
        self.save().await?;

        Ok(entry)
    }

    pub async fn get_by_session_id_scan(&self, session_id: &str) -> Result<SessionEntry> {
        let idx = self.index.read().await;
        idx.entries
            .values()
            .find(|e| e.session_id == session_id)
            .cloned()
            .ok_or_else(|| Error::session_not_found(session_id.to_string()))
    }

    pub async fn init_session_state(
        &self,
        session_key: &str,
        now_ts_ms: i64,
        local_day: &str,
        idle_reset_after_ms: Option<i64>,
    ) -> Result<InitOutcome> {
        {
            let idx = self.index.read().await;
            if let Some(e) = idx.entries.get(session_key) {
                let needs_write = self.needs_reset(e, now_ts_ms, local_day, idle_reset_after_ms)
                    != ResetReason::None
                    || self.needs_activity_update(e, now_ts_ms, local_day, idle_reset_after_ms);

                if !needs_write {
                    return Ok(InitOutcome {
                        entry: e.clone(),
                        did_create: false,
                        did_rotate_session_id: false,
                        reason: ResetReason::None,
                        prev_session_id: None,
                    });
                }
            }
        }

        let mut idx = self.index.write().await;

        let mut did_create = false;
        let mut did_rotate = false;

        let entry = idx
            .entries
            .entry(session_key.to_string())
            .or_insert_with(|| {
                did_create = true;
                SessionEntry {
                    session_key: session_key.to_string(),
                    session_id: new_session_id(),
                    model: None,
                    flags: SessionFlags::default(),
                    counters: SessionCounters::default(),
                }
            });

        let reason = self.needs_reset(entry, now_ts_ms, local_day, idle_reset_after_ms);
        let mut prev_session_id = None;
        if reason != ResetReason::None {
            prev_session_id = Some(entry.session_id.clone());
            entry.session_id = new_session_id();
            entry.counters.last_reset_ts_ms = Some(now_ts_ms);
            entry.counters.reset_count = entry.counters.reset_count.saturating_add(1);

            // Reset per-transcript counters
            entry.counters.approx_tokens = 0;

            // Reset compaction / flush state for new transcript
            entry.counters.compaction_count = 0;
            entry.counters.last_compaction_ts_ms = None;
            entry.counters.tokens_at_last_compaction = None;
            entry.counters.last_memory_flush_ts_ms = None;
            entry.counters.memory_flush_for_compaction_seq = None;

            did_rotate = true;
        }

        entry.counters.last_activity_ts_ms = now_ts_ms;
        entry.counters.last_activity_local_day = Some(local_day.to_string());

        let out = InitOutcome {
            entry: entry.clone(),
            did_create,
            did_rotate_session_id: did_rotate,
            reason,
            prev_session_id,
        };

        drop(idx);
        self.save().await?;
        Ok(out)
    }

    pub async fn reset_session(
        &self,
        session_key: &str,
        now_ts_ms: i64,
        local_day: &str,
    ) -> Result<SessionEntry> {
        let mut idx = self.index.write().await;
        let entry = idx
            .entries
            .entry(session_key.to_string())
            .or_insert_with(|| SessionEntry {
                session_key: session_key.to_string(),
                session_id: new_session_id(),
                model: None,
                flags: SessionFlags::default(),
                counters: SessionCounters::default(),
            });

        entry.session_id = new_session_id();
        entry.counters.last_reset_ts_ms = Some(now_ts_ms);
        entry.counters.reset_count = entry.counters.reset_count.saturating_add(1);

        entry.counters.approx_tokens = 0;
        entry.counters.compaction_count = 0;
        entry.counters.last_compaction_ts_ms = None;
        entry.counters.tokens_at_last_compaction = None;
        entry.counters.last_memory_flush_ts_ms = None;
        entry.counters.memory_flush_for_compaction_seq = None;

        entry.counters.last_activity_ts_ms = now_ts_ms;
        entry.counters.last_activity_local_day = Some(local_day.to_string());

        let out = entry.clone();
        drop(idx);
        self.save().await?;
        Ok(out)
    }

    /// Mark that a pre-compaction memory flush has completed successfully.
    /// You should call this after the flush turn finishes and you wrote the daily memory file.
    pub async fn mark_memory_flush_done(&self, session_key: &str, now_ts_ms: i64) -> Result<()> {
        let mut idx = self.index.write().await;
        let entry = idx
            .entries
            .get_mut(session_key)
            .ok_or_else(|| Error::io(format!("session not found: {session_key}")))?;

        entry.counters.last_memory_flush_ts_ms = Some(now_ts_ms);
        entry.counters.memory_flush_for_compaction_seq = Some(entry.counters.compaction_count);

        drop(idx);
        self.save().await
    }

    /// Returns true if you should run a pre-compaction memory flush *now*.
    ///
    /// Typical policy:
    /// - Only if compaction is enabled
    /// - Only if approx_tokens >= flush_threshold_tokens
    /// - Only once per compaction cycle (guarded by `memory_flush_for_compaction_seq`)
    pub async fn should_run_memory_flush(
        &self,
        session_key: &str,
        approx_tokens: usize,
        flush_threshold_tokens: usize,
    ) -> Result<bool> {
        let idx = self.index.read().await;
        let entry = idx
            .entries
            .get(session_key)
            .ok_or_else(|| Error::io(format!("session not found: {session_key}")))?;

        if !entry.flags.compaction_enabled {
            return Ok(false);
        }
        if approx_tokens < flush_threshold_tokens {
            return Ok(false);
        }

        // Only once per compaction cycle.
        let already =
            entry.counters.memory_flush_for_compaction_seq == Some(entry.counters.compaction_count);
        Ok(!already)
    }

    /// Mark that compaction has completed successfully (i.e. transcript now contains a compaction entry).
    pub async fn mark_compaction_done(
        &self,
        session_key: &str,
        now_ts_ms: i64,
        tokens_before_compaction: usize,
        _reason: CompactionReason,
    ) -> Result<()> {
        let mut idx = self.index.write().await;
        let entry = idx
            .entries
            .get_mut(session_key)
            .ok_or_else(|| Error::io(format!("session not found: {session_key}")))?;

        entry.counters.compaction_count = entry.counters.compaction_count.saturating_add(1);
        entry.counters.last_compaction_ts_ms = Some(now_ts_ms);
        entry.counters.tokens_at_last_compaction = Some(tokens_before_compaction);

        // After compaction, allow a future flush for the next cycle.
        // (Flush guard compares to compaction_count, so bumping compaction_count implicitly resets the guard.)
        // You can leave last_memory_flush_ts_ms as-is for auditing.

        drop(idx);
        self.save().await
    }

    /// Optional helper if you want SessionStore to be the source of truth for approx_tokens.
    pub async fn set_approx_tokens(&self, session_key: &str, approx_tokens: usize) -> Result<()> {
        let mut idx = self.index.write().await;
        let entry = idx
            .entries
            .get_mut(session_key)
            .ok_or_else(|| Error::io(format!("session not found: {session_key}")))?;
        entry.counters.approx_tokens = approx_tokens;
        drop(idx);
        self.save().await
    }

    fn needs_activity_update(
        &self,
        entry: &SessionEntry,
        now_ts_ms: i64,
        local_day: &str,
        idle_reset_after_ms: Option<i64>,
    ) -> bool {
        // Always update if the day changed
        if entry.counters.last_activity_local_day.as_deref() != Some(local_day) {
            return true;
        }

        let last = entry.counters.last_activity_ts_ms;

        // If we never recorded activity before
        if last == 0 {
            return true;
        }

        match idle_reset_after_ms {
            Some(threshold) => {
                let delta = now_ts_ms.saturating_sub(last);
                delta >= threshold
            }
            None => {
                // If idle reset is disabled, only update on day change
                false
            }
        }
    }

    fn needs_reset(
        &self,
        entry: &SessionEntry,
        now_ts_ms: i64,
        local_day: &str,
        idle_reset_after_ms: Option<i64>,
    ) -> ResetReason {
        if entry.flags.daily_reset {
            if let Some(prev_day) = entry.counters.last_activity_local_day.as_deref() {
                if prev_day != local_day {
                    return ResetReason::Daily;
                }
            }
        }

        if let Some(threshold) = idle_reset_after_ms {
            let last = entry.counters.last_activity_ts_ms;
            if last > 0 {
                let inactive = now_ts_ms.saturating_sub(last);
                if inactive >= threshold {
                    return ResetReason::Idle;
                }
            }
        }

        ResetReason::None
    }

    pub async fn should_run_reset_flush(
        &self,
        session_key: &str,
        reason: ResetReason,
    ) -> Result<bool> {
        if reason == ResetReason::None {
            return Ok(false);
        }

        let idx = self.index.read().await;
        let entry = idx
            .entries
            .get(session_key)
            .ok_or_else(|| Error::io(format!("session not found: {session_key}")))?;

        let already = entry.counters.reset_flush_for_reset_seq == Some(entry.counters.reset_count);
        Ok(!already)
    }

    pub async fn mark_reset_flush_done(&self, session_key: &str, now_ts_ms: i64) -> Result<()> {
        let mut idx = self.index.write().await;
        let entry = idx
            .entries
            .get_mut(session_key)
            .ok_or_else(|| Error::io(format!("session not found: {session_key}")))?;

        entry.counters.last_reset_flush_ts_ms = Some(now_ts_ms);
        entry.counters.reset_flush_for_reset_seq = Some(entry.counters.reset_count);

        drop(idx);
        self.save().await
    }

    pub async fn update(&self, entry: &SessionEntry) -> Result<()> {
        {
            let mut idx = self.index.write().await;
            idx.entries.insert(entry.session_key.clone(), entry.clone());
        }
        self.save().await
    }

    pub async fn save(&self) -> Result<()> {
        let (path, snapshot) = {
            let idx = self.index.read().await;
            (self.path.clone(), idx.clone())
        };

        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| Error::io(e.to_string()))?;
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }
}

pub fn new_session_id() -> String {
    use uuid::Uuid;
    Uuid::new_v4().to_string()
}

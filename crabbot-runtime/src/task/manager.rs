use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::queue::scheduler::Priority;
use crate::queue::scheduler::QueueScheduler;
use crate::run::RunReply;
use crate::time::now_ts_ms;

pub type TaskId = String;

const BACKGROUND_THINK_TASK_ID: &str = "background_think";
const BACKGROUND_THINK_INTERVAL_SECS: u64 = 300; // 5 minutes

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Active,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskFlags {
    #[serde(default)]
    pub non_completable: bool,
    #[serde(default)]
    pub non_cancelable: bool,
    #[serde(default)]
    pub non_failurable: bool,
    #[serde(default)]
    pub non_deletable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub created_ts_ms: i64,
    pub updated_ts_ms: i64,

    pub agent_id: String,
    pub description: String,

    /// 0 = one-shot, >0 = repeating.
    pub interval_secs: u64,
    pub next_run_ts_ms: i64,
    pub run_count: u64,

    pub status: TaskStatus,

    #[serde(default)]
    pub notify_session_key: Option<String>,
    #[serde(default)]
    pub notify_agent_id: Option<String>,

    #[serde(default)]
    pub last_run_ts_ms: Option<i64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_output: Option<String>,
    #[serde(default)]
    pub flags: TaskFlags,
}

impl Task {
    pub fn run_session_key(&self) -> String {
        format!("{}:{}", self.agent_id, self.id)
    }

    pub fn tick_body(&self) -> String {
        serde_json::json!({
            "type": "task_tick",
            "task_id": self.id,
            "description": self.description,
        })
        .to_string()
    }

    fn is_protected(&self) -> bool {
        self.flags.non_completable
            || self.flags.non_cancelable
            || self.flags.non_failurable
            || self.flags.non_deletable
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskIndex {
    pub tasks: HashMap<TaskId, Task>,
}

#[derive(Debug, Clone)]
pub struct TaskCreateInput {
    pub agent_id: String,
    pub description: String,
    pub interval_secs: u64,
    pub notify_session_key: Option<String>,
    pub notify_agent_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskManager {
    path: PathBuf,
    index: Arc<Mutex<TaskIndex>>,
    scheduler: Arc<QueueScheduler<RunReply>>,
}

impl TaskManager {
    pub async fn open(folder: PathBuf, scheduler: Arc<QueueScheduler<RunReply>>) -> Result<Self> {
        let path = folder.join("tasks.json");
        let idx = if path.exists() {
            let s = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| Error::io(e.to_string()))?;
            serde_json::from_str(&s).map_err(|e| Error::io(e.to_string()))?
        } else {
            TaskIndex::default()
        };

        let this = Self {
            path,
            index: Arc::new(Mutex::new(idx)),
            scheduler,
        };

        this.ensure_background_think_task().await?;
        Ok(this)
    }

    pub fn spawn_runner(self: Arc<Self>, cancel: CancellationToken) {
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tick.tick() => {}
                }

                // best-effort; don’t crash the runner
                let _ = self.enqueue_due(64).await;
            }
        });
    }

    pub async fn enqueue_due(&self, limit: usize) -> Result<usize> {
        let due = self.claim_due(limit).await?;
        let mut enqueued = 0;
        for t in &due {
            let session_key = t.run_session_key();
            // Only enqueue if not already queued or in-flight for this session.
            // This prevents duplicate heartbeats and interval tasks from piling up.
            if self.scheduler.has_queued_or_inflight(&session_key).await {
                tracing::debug!(
                    "Skipping enqueue for task {} — already queued or in-flight (session: {})",
                    t.id,
                    session_key
                );
                continue;
            }

            let priority = if t.id == BACKGROUND_THINK_TASK_ID {
                Priority::Background
            } else {
                Priority::Normal
            };

            self.scheduler.schedule(session_key, priority).await;
            enqueued += 1;
        }
        Ok(enqueued)
    }

    pub async fn create(&self, input: TaskCreateInput) -> Result<Task> {
        let now_ts_ms = now_ts_ms();
        if input.agent_id.trim().is_empty() {
            return Err(Error::bad_request("tasks.create: agent_id is empty"));
        }
        if input.description.trim().is_empty() {
            return Err(Error::bad_request("tasks.create: description is empty"));
        }

        let id = new_task_id();
        let task = Task {
            id: id.clone(),
            created_ts_ms: now_ts_ms,
            updated_ts_ms: now_ts_ms,
            agent_id: input.agent_id,
            description: input.description,
            interval_secs: input.interval_secs,
            next_run_ts_ms: now_ts_ms,
            run_count: 0,
            status: TaskStatus::Active,
            notify_session_key: input.notify_session_key,
            notify_agent_id: input.notify_agent_id,
            last_run_ts_ms: None,
            last_error: None,
            last_output: None,
            flags: TaskFlags::default(),
        };

        {
            let mut g = self.index.lock().await;
            g.tasks.insert(id, task.clone());
        }
        self.save().await?;

        // optional: enqueue immediately (so create triggers first run without waiting for runner tick)
        let _ = self
            .scheduler
            .add_with_priority(task.run_session_key(), task.tick_body(), Priority::Normal)
            .await;

        Ok(task)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Task>> {
        let g = self.index.lock().await;
        Ok(g.tasks.get(id).cloned())
    }

    pub async fn list(
        &self,
        status: Option<TaskStatus>,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Task>> {
        let limit = limit.clamp(1, 500);
        let g = self.index.lock().await;

        let mut out: Vec<Task> = g
            .tasks
            .values()
            .filter(|t| status.as_ref().map(|s| &t.status == s).unwrap_or(true))
            .filter(|t| agent_id.map(|a| t.agent_id == a).unwrap_or(true))
            .cloned()
            .collect();

        out.sort_by(|a, b| {
            a.next_run_ts_ms
                .cmp(&b.next_run_ts_ms)
                .then(a.created_ts_ms.cmp(&b.created_ts_ms))
        });

        out.truncate(limit);
        Ok(out)
    }

    pub async fn claim_due(&self, limit: usize) -> Result<Vec<Task>> {
        let now_ts_ms = now_ts_ms();
        let limit = limit.clamp(1, 500);
        let mut claimed = Vec::new();

        {
            let mut g = self.index.lock().await;

            let mut ids: Vec<_> = g
                .tasks
                .iter()
                .filter(|(_, t)| t.status == TaskStatus::Active && t.next_run_ts_ms <= now_ts_ms)
                .map(|(id, t)| (id.clone(), t.next_run_ts_ms))
                .collect();

            ids.sort_by(|a, b| a.1.cmp(&b.1));
            ids.truncate(limit);

            for (id, _) in ids {
                if let Some(t) = g.tasks.get_mut(&id) {
                    t.run_count = t.run_count.saturating_add(1);
                    t.last_run_ts_ms = Some(now_ts_ms);
                    t.updated_ts_ms = now_ts_ms;

                    if t.interval_secs > 0 {
                        let delta_ms = (t.interval_secs as i64).saturating_mul(1000);
                        t.next_run_ts_ms = now_ts_ms.saturating_add(delta_ms);
                    } else {
                        t.next_run_ts_ms = i64::MAX;
                    }

                    claimed.push(t.clone());
                }
            }
        }

        if !claimed.is_empty() {
            self.save().await?;
        }
        Ok(claimed)
    }

    pub async fn complete(&self, id: &str, output: Option<String>) -> Result<Task> {
        let now_ts_ms = now_ts_ms();
        let task = {
            let mut g = self.index.lock().await;
            let t = g
                .tasks
                .get_mut(id)
                .ok_or_else(|| Error::other(format!("task not found: {id}")))?;

            if t.flags.non_completable {
                return Err(Error::bad_request(
                    "tasks.complete: task is not completable",
                ));
            }

            t.status = TaskStatus::Completed;
            t.updated_ts_ms = now_ts_ms;
            t.last_error = None;
            t.last_output = output;
            t.next_run_ts_ms = i64::MAX;

            t.clone()
        };

        self.save().await?;
        self.enqueue_completion_notification(&task).await;

        Ok(task)
    }

    pub async fn fail(&self, id: &str, error: String) -> Result<Task> {
        let now_ts_ms = now_ts_ms();
        if error.trim().is_empty() {
            return Err(Error::bad_request("tasks.fail: error is empty"));
        }

        let task = {
            let mut g = self.index.lock().await;
            let t = g
                .tasks
                .get_mut(id)
                .ok_or_else(|| Error::other(format!("task not found: {id}")))?;

            if t.flags.non_failurable {
                return Err(Error::bad_request("tasks.fail: task is not failurable"));
            }
            t.status = TaskStatus::Failed;
            t.updated_ts_ms = now_ts_ms;
            t.last_error = Some(error);
            t.clone()
        };

        self.save().await?;
        Ok(task)
    }

    pub async fn cancel(&self, id: &str, reason: Option<String>) -> Result<Task> {
        let now_ts_ms = now_ts_ms();
        let task = {
            let mut g = self.index.lock().await;
            let t = g
                .tasks
                .get_mut(id)
                .ok_or_else(|| Error::other(format!("task not found: {id}")))?;

            if t.flags.non_cancelable {
                return Err(Error::bad_request("tasks.cancel: task is not cancelable"));
            }

            t.status = TaskStatus::Canceled;
            t.updated_ts_ms = now_ts_ms;
            t.last_error = reason;
            t.next_run_ts_ms = i64::MAX;
            t.clone()
        };

        self.save().await?;
        Ok(task)
    }

    async fn enqueue_completion_notification(&self, task: &Task) {
        let key = match task.notify_session_key.as_deref() {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => return,
        };

        // If caller gave an unprefixed key but also a notify_agent_id, apply your prefix convention.
        let notify_key = if key.contains(':') {
            key
        } else if let Some(agent_id) = task.notify_agent_id.as_deref() {
            format!("{agent_id}:{key}")
        } else {
            key
        };

        let desc = task.description.as_str();
        let out = task.last_output.as_deref().unwrap_or("").trim();

        let body = if out.is_empty() {
            format!("Task completed: {desc}")
        } else {
            format!("Task completed: {desc}\n\n{out}")
        };

        let _ = self
            .scheduler
            .add_with_priority(notify_key, body, Priority::Normal)
            .await;
    }

    pub async fn save(&self) -> Result<()> {
        let snapshot = { self.index.lock().await.clone() };
        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| Error::io(e.to_string()))?;

        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| Error::io(e.to_string()))?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| Error::io(e.to_string()))?;
        Ok(())
    }

    async fn ensure_background_think_task(&self) -> Result<()> {
        let now = now_ts_ms();
        let mut created = false;

        {
            let mut g = self.index.lock().await;

            if !g.tasks.contains_key(BACKGROUND_THINK_TASK_ID) {
                let t = Task {
                    id: BACKGROUND_THINK_TASK_ID.to_string(),
                    created_ts_ms: now,
                    updated_ts_ms: now,
                    agent_id: "system".to_string(),
                    description: "Background thinking session. Review current goals, check task progress, update plans, reflect on recent interactions, and decide if any new work should be started. Update short-term memory with your current thinking and priorities. When important information, status updates, or summaries should be surfaced to the user, use the render_user_ui_html tool to update the session's UI HTML so it is visible in the main UI.".to_string(),
                    interval_secs: BACKGROUND_THINK_INTERVAL_SECS,
                    next_run_ts_ms: now + 30_000, // start 30s after boot
                    run_count: 0,
                    status: TaskStatus::Active,
                    notify_session_key: None,
                    notify_agent_id: None,
                    last_run_ts_ms: None,
                    last_error: None,
                    last_output: None,
                    flags: TaskFlags {
                        non_completable: true,
                        non_cancelable: true,
                        non_failurable: true,
                        non_deletable: true,
                    },
                };

                g.tasks.insert(BACKGROUND_THINK_TASK_ID.to_string(), t);
                created = true;
            } else if let Some(t) = g.tasks.get_mut(BACKGROUND_THINK_TASK_ID) {
                t.flags.non_completable = true;
                t.flags.non_cancelable = true;
                t.flags.non_failurable = true;
                t.flags.non_deletable = true;

                if t.status != TaskStatus::Active {
                    t.status = TaskStatus::Active;
                }
                if t.interval_secs == 0 {
                    t.interval_secs = BACKGROUND_THINK_INTERVAL_SECS;
                }
            }
        }

        if created {
            self.save().await?;
        }
        Ok(())
    }
}

fn new_task_id() -> String {
    use uuid::Uuid;
    Uuid::new_v4().to_string()
}

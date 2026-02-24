use crabbot_shared::api::{transcript::TranscriptEvent, ui_html::UiHtmlUpdate};
use serde_json::json;
use std::{collections::HashMap, sync::Arc};

use tokio::sync::{RwLock, broadcast};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::AgentRegistry,
    config::Config,
    error::{Error, Result},
    llm::{
        client::Llm,
        types::{Completion, LlmOutput},
    },
    memory::service::MemoryService,
    queue::scheduler::QueueScheduler,
    routing::router::DefaultSessionRouter,
    storage::{
        session_store::{CompactionReason, InitOutcome, SessionEntry, SessionStore},
        transcript_store::TranscriptStore,
    },
    task::manager::TaskManager,
    tools::{registry::ToolRegistry, tool::ToolSpec},
    ui::store::UiHtmlStore,
};

#[derive(Clone, Debug)]
pub struct RunReply {
    pub session_id: String,
    pub response: String,
}

#[derive(Debug)]
pub struct RunEngine {
    cfg: Config,

    router: Arc<DefaultSessionRouter>,
    scheduler: Arc<QueueScheduler<RunReply>>,

    sessions: Arc<SessionStore>,
    transcripts: Arc<TranscriptStore>,

    transcript_bus: TranscriptBus,

    llm: Arc<dyn Llm>,
    memory: Arc<MemoryService>,

    agents: Arc<AgentRegistry>,

    tasks: Arc<TaskManager>,

    pub html_store: UiHtmlStore,

    #[allow(dead_code)]
    tools: Arc<ToolRegistry>,
}

#[derive(Clone, Debug, Default)]
pub struct TranscriptBus {
    inner: Arc<RwLock<HashMap<String, broadcast::Sender<TranscriptEvent>>>>,
}

impl TranscriptBus {
    async fn sender(&self, session_key: &str) -> broadcast::Sender<TranscriptEvent> {
        let mut g = self.inner.write().await;
        if let Some(tx) = g.get(session_key) {
            return tx.clone();
        }
        let (tx, _rx) = broadcast::channel(512);
        g.insert(session_key.to_string(), tx.clone());
        tx
    }

    pub async fn subscribe(&self, session_key: &str) -> broadcast::Receiver<TranscriptEvent> {
        self.sender(session_key).await.subscribe()
    }

    pub async fn publish(&self, session_key: &str, ev: TranscriptEvent) {
        let tx = self.sender(session_key).await;
        let _ = tx.send(ev);
    }
}

impl RunEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: Config,
        router: Arc<DefaultSessionRouter>,
        scheduler: Arc<QueueScheduler<RunReply>>,
        sessions: Arc<SessionStore>,
        transcripts: Arc<TranscriptStore>,
        llm: Arc<dyn Llm>,
        tools: Arc<ToolRegistry>,
        agents: Arc<AgentRegistry>,
        memory: Arc<MemoryService>,
        tasks: Arc<TaskManager>,
        html_store: UiHtmlStore,
    ) -> Self {
        Self {
            cfg,
            router,
            scheduler,
            sessions,
            transcripts,
            transcript_bus: TranscriptBus::default(),
            llm,
            memory,
            tools,
            tasks,
            agents,
            html_store,
        }
    }

    pub async fn get_ui_html(&self, session_key: &str) -> Result<(bool, String)> {
        self.html_store.load(session_key).await
    }

    pub async fn subscribe_ui_html(
        &self,
        session_key: &str,
    ) -> Result<tokio::sync::broadcast::Receiver<UiHtmlUpdate>> {
        Ok(self.html_store.subscribe(session_key).await)
    }

    pub async fn subscribe_transcript(
        &self,
        session_key: &str,
    ) -> Result<(String, broadcast::Receiver<TranscriptEvent>)> {
        let session = self.sessions.get(session_key).await?;
        let rx = self.transcript_bus.subscribe(session_key).await;
        Ok((session.session_id.clone(), rx))
    }

    pub async fn subscribe_transcript_by_session_id(
        &self,
        session_id: &str,
    ) -> broadcast::Receiver<TranscriptEvent> {
        self.transcript_bus.subscribe(session_id).await
    }

    pub async fn handle_message(&self, session_key: String, body: String) -> Result<RunReply> {
        let rx = self.scheduler.add(session_key, body).await;
        rx.await
            .map_err(|_| Error::other("response channel dropped"))?
    }

    pub async fn run(&self, cancel: CancellationToken) -> Result<()> {
        self.tasks.clone().spawn_runner(cancel.clone());

        loop {
            let item = match self.scheduler.next_ready(&cancel).await? {
                Some(it) => it,
                None => break,
            };

            let res = self.process(&item.session_key, &item.body).await;
            // if error log it
            if let Err(err) = &res {
                tracing::error!("Error processing message: {}", err);
            }
            self.scheduler.complete(item, res).await;
        }
        Ok(())
    }

    pub async fn list_session_keys(&self) -> Result<Vec<String>> {
        self.sessions.list_session_keys().await
    }

    pub async fn get_transcript(
        &self,
        session_key: &str,
        after_ts_ms: Option<i64>,
        limit: Option<usize>,
    ) -> Result<(String, Vec<TranscriptEvent>)> {
        let session = self.sessions.get(session_key).await?;

        let mut events = self
            .transcripts
            .read_tail(
                &session.session_id,
                limit.unwrap_or(self.cfg.prompt.max_history_events),
            )
            .await?;

        if let Some(after) = after_ts_ms {
            events.retain(|e| e.ts_ms() > after);
        }

        Ok((session.session_id.clone(), events))
    }

    async fn append_and_publish(
        &self,
        session_key: &str,
        session_id: &str,
        ev: TranscriptEvent,
    ) -> Result<()> {
        self.transcripts.append(session_id, ev.clone()).await?;
        self.transcript_bus.publish(session_key, ev).await;
        Ok(())
    }

    fn approx_tokens_via_llm(&self, session: &SessionEntry, events: &[TranscriptEvent]) -> usize {
        let chars = self.llm.estimate_prompt_chars(session, events, "");
        (chars + 3) / 4
    }

    async fn maybe_memory_flush(
        &self,
        session: &SessionEntry,
        tail_for_prompt: &[TranscriptEvent],
        now_ts_ms: i64,
        local_day: &str,
    ) -> Result<()> {
        if !session.flags.compaction_enabled {
            return Ok(());
        }

        let approx = self.approx_tokens_via_llm(session, tail_for_prompt);
        self.sessions
            .set_approx_tokens(&session.session_key, approx)
            .await?;

        let flush_threshold = self.cfg.prompt.flush_threshold_tokens;
        if !self
            .sessions
            .should_run_memory_flush(&session.session_key, approx, flush_threshold)
            .await?
        {
            return Ok(());
        }

        //log reason
        tracing::info!(
            "Mamory flush for session {} because it has {} chars",
            session.session_key,
            approx
        );

        let res = crate::memory::flush::run_memory_flush(
            self.llm.as_ref(),
            session,
            tail_for_prompt,
            self.cfg.memory.flush_context_max_chars,
        )
        .await?;

        crate::memory::flush::apply_flush_result(self.memory.as_ref(), local_day, res).await?;

        self.sessions
            .mark_memory_flush_done(&session.session_key, now_ts_ms)
            .await?;

        Ok(())
    }

    async fn maybe_reset_flush(
        &self,
        session: &SessionEntry,
        prev_session_id: &str,
        reason: crate::storage::session_store::ResetReason,
        now_ts_ms: i64,
        local_day: &str,
    ) -> Result<()> {
        if !self
            .sessions
            .should_run_reset_flush(&session.session_key, reason)
            .await?
        {
            return Ok(());
        }

        let tail = self
            .transcripts
            .read_for_prompt(prev_session_id, self.cfg.prompt.max_history_events)
            .await?;

        // Gate: only flush if we have "enough" content.
        // Use existing flush_context_max_chars as the threshold.
        let approx = TranscriptEvent::estimate_prompt_chars(&tail, "");
        if approx < (self.cfg.memory.flush_context_max_chars / 4).max(512) {
            return Ok(());
        }
        //log reason
        tracing::info!(
            "Resetting flush for session {} because it has {} chars",
            session.session_key,
            approx
        );

        let res = crate::memory::flush::run_memory_flush(
            self.llm.as_ref(),
            session,
            &tail,
            self.cfg.memory.flush_context_max_chars,
        )
        .await?;

        crate::memory::flush::apply_flush_result(self.memory.as_ref(), local_day, res).await?;

        self.sessions
            .mark_reset_flush_done(&session.session_key, now_ts_ms)
            .await?;

        Ok(())
    }

    async fn compact_if_needed(
        &self,
        session: &SessionEntry,
        now_ts_ms: i64,
        local_day: &str,
    ) -> Result<()> {
        if !session.flags.compaction_enabled {
            return Ok(());
        }

        let tail_for_prompt = self
            .transcripts
            .read_for_prompt(&session.session_id, self.cfg.prompt.max_history_events)
            .await?;

        let approx = self.approx_tokens_via_llm(session, &tail_for_prompt);
        self.sessions
            .set_approx_tokens(&session.session_key, approx)
            .await?;

        if approx < self.cfg.prompt.compaction_threshold_tokens {
            let n = self.transcripts.count_events(&session.session_id).await?;
            if n <= self.cfg.prompt.max_history_events {
                return Ok(());
            }
        }

        let (compacted_events, covers_up_to_ts_ms) = self
            .transcripts
            .get_compact_events(&session.session_id, self.cfg.prompt.max_history_events)
            .await?;

        // Automatic memory flush before compaction (provider-agnostic)
        self.maybe_memory_flush(session, &tail_for_prompt, now_ts_ms, local_day)
            .await?;

        let summary_text = self.llm.compact(session, &compacted_events).await?;
        let summary_ev = TranscriptEvent::compaction_summary(summary_text, covers_up_to_ts_ms);

        self.transcripts
            .replace(
                &session.session_id,
                compacted_events.len(),
                summary_ev.clone(),
            )
            .await?;

        self.transcript_bus
            .publish(&session.session_key, summary_ev)
            .await;

        self.append_and_publish(
            &session.session_key,
            &session.session_id,
            TranscriptEvent::custom_note("control.reload_required", json!({"reason":"compaction"})),
        )
        .await?;

        self.sessions
            .mark_compaction_done(
                &session.session_key,
                now_ts_ms,
                approx,
                CompactionReason::TokenLimit,
            )
            .await?;

        Ok(())
    }

    async fn process(&self, session_key: &str, body: &str) -> Result<RunReply> {
        tracing::info!("Processing session {}", session_key);
        let now_ts_ms = crate::time::now_ts_ms();
        let local_day = crate::time::local_day_string();
        let idle_after_ms = self.cfg.idle_reset_after_ms;

        let route = self.router.route(session_key)?;

        let agent = self
            .agents
            .get(&route.agent_id)
            .await
            .ok_or_else(|| Error::other(format!("agent not found: {}", route.agent_id)))?;

        let InitOutcome {
            entry: session,
            did_rotate_session_id,
            reason,
            prev_session_id,
            ..
        } = self
            .sessions
            .init_session_state(session_key, now_ts_ms, &local_day, idle_after_ms)
            .await?;

        if did_rotate_session_id {
            if let Some(old_id) = prev_session_id.as_deref() {
                self.maybe_reset_flush(&session, old_id, reason, now_ts_ms, &local_day)
                    .await?;
            }

            self.append_and_publish(
                &session.session_key,
                &session.session_id,
                TranscriptEvent::custom_note(
                    "control.session_reset",
                    json!({ "session_key": session_key, "reason": format!("{reason:?}") }),
                ),
            )
            .await?;
        }

        self.append_and_publish(
            &session.session_key,
            &session.session_id,
            TranscriptEvent::user(body.to_string()),
        )
        .await?;

        self.compact_if_needed(&session, now_ts_ms, &local_day)
            .await?;

        let history = self
            .transcripts
            .read_for_prompt(&session.session_id, self.cfg.prompt.max_history_events)
            .await?;

        let tools: Vec<ToolSpec> = self.tools.tool_specs().await;

        let md_events = crate::agent::prompt::load_md_events(&self.cfg.paths, &agent).await?;
        let mut working_events: Vec<TranscriptEvent> = Vec::new();
        working_events.extend(md_events);
        working_events.extend(
            self.memory
                .build_prompt_events(
                    agent.include_long_term_memory,
                    agent.include_daily_memory_days,
                    agent.memory_prompt_max_chars,
                )
                .await?,
        );
        working_events.extend(history);

        let mut final_text: Option<String> = None;
        let mut steps = 0usize;
        let max_steps = 16usize;

        while steps < max_steps {
            steps += 1;

            let completion: Completion =
                self.llm.complete(&session, &working_events, &tools).await?;
            let mut saw_tool_call = false;
            tracing::info!("Completion: {:?}", completion);
            for out in completion.outputs {
                match out {
                    LlmOutput::AssistantText(text) => {
                        final_text = Some(match final_text {
                            None => text.clone(),
                            Some(prev) => format!("{prev}\n{text}"),
                        });

                        working_events.push(TranscriptEvent::assistant(text));
                    }
                    LlmOutput::ToolCall(tool_call) => {
                        saw_tool_call = true;

                        let tc_ev = TranscriptEvent::tool_call(
                            tool_call.name.clone(),
                            tool_call.id.clone(),
                            tool_call.args.clone(),
                        );

                        self.append_and_publish(
                            &session.session_key,
                            &session.session_id,
                            tc_ev.clone(),
                        )
                        .await?;
                        working_events.push(tc_ev);

                        let tool = self.tools.get(&tool_call.name).await.ok_or_else(|| {
                            Error::tool(format!("unknown tool: {}", tool_call.name))
                        })?;

                        match tool.call(tool_call.clone(), session_key).await {
                            Ok(res_json) => {
                                let tr_ev = TranscriptEvent::tool_result(
                                    tool_call.id.clone(),
                                    true,
                                    res_json.output.clone(),
                                    res_json.error.clone(),
                                );

                                self.append_and_publish(
                                    &session.session_key,
                                    &session.session_id,
                                    tr_ev.clone(),
                                )
                                .await?;
                                working_events.push(tr_ev);
                            }
                            Err(err) => {
                                let tr_ev = TranscriptEvent::tool_result(
                                    tool_call.id.clone(),
                                    false,
                                    json!({}),
                                    Some(err.to_string()),
                                );

                                self.append_and_publish(
                                    &session.session_key,
                                    &session.session_id,
                                    tr_ev.clone(),
                                )
                                .await?;
                                working_events.push(tr_ev);
                            }
                        }
                    }
                }
            }

            if !saw_tool_call {
                break;
            }
        }

        let response = final_text.unwrap_or_default();

        self.append_and_publish(
            &session.session_key,
            &session.session_id,
            TranscriptEvent::assistant(response.clone()),
        )
        .await?;

        Ok(RunReply {
            session_id: session.session_id.clone(),
            response,
        })
    }
}

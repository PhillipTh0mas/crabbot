use crabbot_shared::api::{transcript::TranscriptEvent, ui_html::UiHtmlUpdate};
use serde_json::json;
use std::{collections::HashMap, sync::Arc};

use tokio::sync::{RwLock, broadcast};
use tokio_util::sync::CancellationToken;
use tracing;

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
    tools::{
        registry::ToolRegistry,
        tool::ToolSpec,
        tool_agent::{self, ToolAgentService},
        tool_sessions::ToolSessionStore,
    },
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
    pub memory: Arc<MemoryService>,

    agents: Arc<AgentRegistry>,

    tasks: Arc<TaskManager>,

    pub html_store: UiHtmlStore,

    tool_sessions: Arc<ToolSessionStore>,
    tool_agent: Arc<ToolAgentService>,

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
        tool_sessions: Arc<ToolSessionStore>,
    ) -> Self {
        let tool_agent = Arc::new(ToolAgentService::new(
            llm.clone(),
            tools.clone(),
            tool_sessions.clone(),
        ));

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
            tool_sessions,
            tool_agent,
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
        // 1. Resolve/init the session so the user message is visible immediately.
        let now_ts_ms = crate::time::now_ts_ms();
        let local_day = crate::time::local_day_string();
        let idle_after_ms = self.cfg.idle_reset_after_ms;

        let InitOutcome {
            entry: session,
            did_rotate_session_id,
            reason,
            prev_session_id,
            ..
        } = self
            .sessions
            .init_session_state(&session_key, now_ts_ms, &local_day, idle_after_ms)
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
                    json!({ "session_key": &session_key, "reason": format!("{reason:?}") }),
                ),
            )
            .await?;
        }

        // 2. Append the user message to the transcript right away so the UI sees it.
        self.append_and_publish(
            &session.session_key,
            &session.session_id,
            TranscriptEvent::user(body),
        )
        .await?;

        // 3. Schedule the session for LLM processing; await the eventual response.
        let rx = self.scheduler.add(session_key, String::new()).await;
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

            let has_waiter = item.has_waiter();
            let session_key = item.session_key.clone();
            let priority = item.priority;

            let res = self.process_step(&item.session_key).await;

            match res {
                Ok(Some(reply)) => {
                    // Final response produced — complete the item and notify the waiter.
                    self.scheduler.complete(item, Ok(reply)).await;
                }
                Ok(None) => {
                    // Step completed but more work remains (e.g. tool calls appended,
                    // need another LLM turn). Yield the slot and re-schedule.
                    if has_waiter {
                        // Carry the waiter's resp_id forward so it gets resolved later.
                        let resp_id = self.scheduler.complete_step(item).await;
                        self.scheduler
                            .reschedule_with_resp(session_key, resp_id, priority)
                            .await;
                    } else {
                        let _ = self.scheduler.complete_step(item).await;
                        self.scheduler.schedule(session_key, priority).await;
                    }
                }
                Err(err) => {
                    tracing::error!("Error processing session '{}': {}", session_key, err);
                    self.scheduler.complete(item, Err(err)).await;
                }
            }
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

    /// Single-step process: run one LLM completion for the session.
    ///
    /// - Returns `Ok(Some(reply))` when the LLM produced a final text response (no tool calls).
    /// - Returns `Ok(None)` when tool calls were executed and results appended to the transcript;
    ///   the caller should re-schedule the session for another step.
    async fn process_step(&self, session_key: &str) -> Result<Option<RunReply>> {
        tracing::info!("Processing step for session '{}'", session_key);
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

        let is_background_think = session_key.contains(":background_think");
        let is_task = session_key.starts_with("system:") || session_key.contains(":task_");

        // For background think and task sessions, inject context on the FIRST step
        // (i.e. when the last transcript event is not an assistant or tool_result message,
        //  meaning the LLM hasn't started responding yet in this cycle).
        if is_background_think || is_task {
            let tail = self.transcripts.read_tail(&session.session_id, 1).await?;
            let needs_injection = tail.last().map_or(true, |ev| {
                !matches!(
                    ev,
                    TranscriptEvent::ToolResult(_) | TranscriptEvent::Assistant(_)
                )
            });

            if needs_injection {
                if is_background_think {
                    let cross_session_context =
                        self.build_background_think_context(session_key).await;

                    let think_prompt = format!(
                        "You are in a background thinking cycle. You have access to the `use_tool` function — you MUST call it.\n\n\
                         ## How to call tools\n\
                         You have ONE function available: `use_tool(tool_name, prompt)`.\n\
                         - `tool_name`: the name of the tool (e.g. \"memory\", \"tasks\", \"render_user_ui_html\")\n\
                         - `prompt`: a natural language description of what you want the tool to do\n\n\
                         Examples:\n\
                         - use_tool(tool_name=\"memory\", prompt=\"Read my short-term memory using op get_short_term\")\n\
                         - use_tool(tool_name=\"memory\", prompt=\"Replace short-term memory with: <your updated notes>\")\n\
                         - use_tool(tool_name=\"memory\", prompt=\"Save to indexed long-term memory: <durable fact>\")\n\
                         - use_tool(tool_name=\"tasks\", prompt=\"List all active tasks\")\n\
                         - use_tool(tool_name=\"tasks\", prompt=\"Create a new task: <description>\")\n\
                         - use_tool(tool_name=\"render_user_ui_html\", prompt=\"Save this HTML for the default session: <html>\")\n\n\
                         ## What happened recently across all sessions\n\
                         {cross_session_context}\n\n\
                         ## Your job right now — follow these steps IN ORDER, calling use_tool for each:\n\
                         1. **Read short-term memory** — call use_tool with tool_name=\"memory\" and prompt=\"Read short-term memory (op get_short_term)\". This is your scratchpad from the last cycle.\n\
                         2. **Reflect** on the recent activity above and your short-term memory. What's new? What changed? What matters?\n\
                         3. **Update short-term memory** — call use_tool with tool_name=\"memory\" and prompt=\"Replace short-term memory with the following updated content: <your updated thinking, priorities, key facts, and any notes for next cycle>\". This is your ONLY persistent state between cycles, so be thorough.\n\
                         4. **Check tasks** — call use_tool with tool_name=\"tasks\" and prompt=\"List all active tasks\". Review them. Complete, fail, or cancel any that are done.\n\
                         5. **Save durable facts** — if you learned something that should persist beyond short-term memory (user preferences, project info, recurring patterns), call use_tool with tool_name=\"memory\" and prompt=\"Save to indexed memory: <the fact>\".\n\
                         6. **Surface info to user** — if there's something the user should see (status update, summary, notification), call use_tool with tool_name=\"render_user_ui_html\" and prompt=\"Save HTML for the default session: <tailwind-styled html>\".\n\
                         7. **Create tasks** if needed — if you identify work that should happen (monitoring, follow-ups, reminders), call use_tool with tool_name=\"tasks\" and prompt=\"Create a task: <description>\".\n\n\
                         CRITICAL RULES:\n\
                         - You MUST actually call use_tool. Do NOT just describe what you would do.\n\
                         - Start with step 1 (read short-term memory). Then proceed through the steps.\n\
                         - If there is no new activity and nothing to update, at minimum read your short-term memory and confirm it is current.\n\
                         - Each use_tool call will be executed by a specialised tool agent that knows the tool's API."
                    );

                    self.append_and_publish(
                        &session.session_key,
                        &session.session_id,
                        TranscriptEvent::custom_message("system", think_prompt),
                    )
                    .await?;
                } else if is_task {
                    // Regular task session: look up the task and inject its description
                    // as a system message so the agent knows what to do.
                    let task_id = session_key
                        .split(':')
                        .last()
                        .unwrap_or(session_key)
                        .to_string();

                    let task_desc = match self.tasks.get(&task_id).await {
                        Ok(Some(t)) => t.description.clone(),
                        _ => format!("Execute task: {task_id}"),
                    };

                    self.append_and_publish(
                        &session.session_key,
                        &session.session_id,
                        TranscriptEvent::custom_message(
                            "system",
                            format!(
                                "Task tick for '{task_id}'.\n\nTask description: {task_desc}\n\n\
                                 Execute this task. Use tools as needed. \
                                 When finished, report the result."
                            ),
                        ),
                    )
                    .await?;
                }
            }
        }

        self.compact_if_needed(&session, now_ts_ms, &local_day)
            .await?;

        let history = self
            .transcripts
            .read_for_prompt(&session.session_id, self.cfg.prompt.max_history_events)
            .await?;

        // Safety valve: count consecutive tool-call steps in this cycle.
        // A "step" is a ToolResult event that isn't preceded by a User event
        // (i.e. the LLM has been looping on tool calls without new user input).
        // If we exceed the limit, run the LLM without tools so it MUST produce text.
        const MAX_TOOL_STEPS: usize = 16;
        let consecutive_tool_steps = {
            let mut count = 0usize;
            for ev in history.iter().rev() {
                match ev {
                    TranscriptEvent::ToolResult(_) | TranscriptEvent::ToolCall(_) => {
                        count += 1;
                    }
                    TranscriptEvent::User(_) | TranscriptEvent::Assistant(_) => break,
                    _ => {} // skip notes, summaries, custom messages
                }
            }
            // Each tool exchange is a Call+Result pair, so divide by 2 for "steps"
            count / 2
        };

        let at_step_limit = consecutive_tool_steps >= MAX_TOOL_STEPS;
        if at_step_limit {
            tracing::warn!(
                "Session '{}' hit max tool steps ({}/{}), forcing final response",
                session_key,
                consecutive_tool_steps,
                MAX_TOOL_STEPS
            );
        }

        // Build the single `use_tool` spec — the calling agent only sees this,
        // not the full JSON schemas of every tool.
        // If we've hit the step limit, omit tools to force a text-only response.
        let tools: Vec<ToolSpec> = if at_step_limit {
            vec![]
        } else {
            match self.tool_agent.use_tool_spec().await {
                Some(spec) => vec![spec],
                None => vec![],
            }
        };

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

        // --- Single LLM completion ---
        let completion: Completion = self.llm.complete(&session, &working_events, &tools).await?;

        let mut final_text: Option<String> = None;
        let mut saw_tool_call = false;
        tracing::info!("Completion: {:?}", completion);

        for out in completion.outputs {
            match out {
                LlmOutput::AssistantText(text) => {
                    final_text = Some(match final_text {
                        None => text.clone(),
                        Some(prev) => format!("{prev}\n{text}"),
                    });
                    // We'll persist this after the loop once we know if there were tool calls.
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

                    let result_text = if tool_call.name == "use_tool" {
                        match tool_agent::parse_use_tool_args(&tool_call.args) {
                            Ok((tool_name, prompt)) => {
                                match self
                                    .tool_agent
                                    .handle(session_key, &tool_name, &prompt)
                                    .await
                                {
                                    Ok(summary) => summary,
                                    Err(err) => {
                                        format!("[Tool agent error: {}]", err)
                                    }
                                }
                            }
                            Err(err) => {
                                format!("[Invalid use_tool arguments: {}]", err)
                            }
                        }
                    } else {
                        format!(
                            "[Unknown function '{}'. Use `use_tool` with a tool_name and prompt instead.]",
                            tool_call.name
                        )
                    };

                    let tr_ev = TranscriptEvent::tool_result(
                        tool_call.id.clone(),
                        true,
                        json!(result_text),
                        None,
                    );

                    self.append_and_publish(
                        &session.session_key,
                        &session.session_id,
                        tr_ev.clone(),
                    )
                    .await?;
                }
            }
        }

        if saw_tool_call {
            // Tool calls were executed and results are in the transcript.
            // Yield back to the scheduler — return None so we get re-scheduled
            // for another LLM turn that sees the tool results.
            return Ok(None);
        }

        // No tool calls — this is a final text response.
        let response = final_text.unwrap_or_default();

        self.append_and_publish(
            &session.session_key,
            &session.session_id,
            TranscriptEvent::assistant(response.clone()),
        )
        .await?;

        // Post-response memory flush: for user-facing sessions, flush to memory
        // at a much lower threshold so short conversations still get persisted.
        if !is_background_think && session.flags.compaction_enabled {
            let post_tail = self
                .transcripts
                .read_for_prompt(&session.session_id, self.cfg.prompt.max_history_events)
                .await
                .unwrap_or_default();
            let post_approx = self.approx_tokens_via_llm(&session, &post_tail);

            const POST_RESPONSE_FLUSH_MIN_TOKENS: usize = 2_000;
            if post_approx >= POST_RESPONSE_FLUSH_MIN_TOKENS {
                let local_day = crate::time::local_day_string();
                match crate::memory::flush::run_memory_flush(
                    self.llm.as_ref(),
                    &session,
                    &post_tail,
                    self.cfg.memory.flush_context_max_chars,
                )
                .await
                {
                    Ok(res) => {
                        if !res.daily.is_empty() || !res.long_term.is_empty() {
                            tracing::info!(
                                "Post-response memory flush for session '{}': {} daily, {} long_term items",
                                session.session_key,
                                res.daily.len(),
                                res.long_term.len(),
                            );
                            if let Err(e) = crate::memory::flush::apply_flush_result(
                                self.memory.as_ref(),
                                &local_day,
                                res,
                            )
                            .await
                            {
                                tracing::warn!(
                                    "Post-response memory flush apply failed for '{}': {e}",
                                    session.session_key
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Post-response memory flush failed for '{}': {e}",
                            session.session_key
                        );
                    }
                }
            }
        }

        Ok(Some(RunReply {
            session_id: session.session_id.clone(),
            response,
        }))
    }

    pub async fn list_sessions_detailed(
        &self,
    ) -> Result<Vec<crabbot_shared::api::model::SessionInfo>> {
        let keys = self.sessions.list_session_keys().await?;
        let mut sessions = Vec::new();
        for key in keys {
            let (session_type, label) = classify_session_key(&key);
            sessions.push(crabbot_shared::api::model::SessionInfo {
                key,
                session_type,
                label,
            });
        }
        Ok(sessions)
    }

    pub async fn get_in_flight_sessions(&self) -> Result<Vec<String>> {
        let set = self.scheduler.in_flight_sessions().await;
        Ok(set.into_iter().collect())
    }

    pub async fn get_tool_sessions_stats(
        &self,
    ) -> Result<Vec<crabbot_shared::api::model::ToolSessionStatsResp>> {
        let names = self.tool_sessions.list_tool_names().await;
        let mut out = Vec::new();
        for name in names {
            if let Some(stats) = self.tool_sessions.get_stats(&name).await {
                out.push(crabbot_shared::api::model::ToolSessionStatsResp {
                    tool_name: stats.tool_name,
                    total_calls: stats.total_calls,
                    total_errors: stats.total_errors,
                    entry_count: stats.entry_count,
                    compaction_count: stats.compaction_count,
                    last_call_ts_ms: stats.last_call_ts_ms,
                });
            }
        }
        Ok(out)
    }

    pub async fn get_tool_session_history(
        &self,
        tool_name: &str,
    ) -> Result<Option<crabbot_shared::api::model::ToolSessionHistoryResp>> {
        let session = self.tool_sessions.get_session(tool_name).await;
        Ok(session.map(|s| {
            let entries = s
                .entries
                .iter()
                .map(|e| {
                    use crabbot_shared::api::model::{
                        ToolSessionEntryKindResp, ToolSessionEntryResp,
                    };
                    let kind = match &e.kind {
                        crate::tools::tool_sessions::ToolSessionEntryKind::Call {
                            call_id,
                            args_summary,
                        } => ToolSessionEntryKindResp::Call {
                            call_id: call_id.clone(),
                            args_summary: args_summary.clone(),
                        },
                        crate::tools::tool_sessions::ToolSessionEntryKind::Result {
                            call_id,
                            success,
                            output_summary,
                        } => ToolSessionEntryKindResp::Result {
                            call_id: call_id.clone(),
                            success: *success,
                            output_summary: output_summary.clone(),
                        },
                        crate::tools::tool_sessions::ToolSessionEntryKind::Error {
                            call_id,
                            error,
                        } => ToolSessionEntryKindResp::Error {
                            call_id: call_id.clone(),
                            error: error.clone(),
                        },
                        crate::tools::tool_sessions::ToolSessionEntryKind::CompactionSummary {
                            summary,
                            covers_up_to_ts_ms,
                            entries_compacted,
                        } => ToolSessionEntryKindResp::CompactionSummary {
                            summary: summary.clone(),
                            covers_up_to_ts_ms: *covers_up_to_ts_ms,
                            entries_compacted: *entries_compacted,
                        },
                        crate::tools::tool_sessions::ToolSessionEntryKind::Note { text } => {
                            ToolSessionEntryKindResp::Note { text: text.clone() }
                        }
                    };
                    ToolSessionEntryResp {
                        ts_ms: e.ts_ms,
                        kind,
                    }
                })
                .collect();
            crabbot_shared::api::model::ToolSessionHistoryResp {
                tool_name: s.tool_name,
                total_calls: s.total_calls,
                total_errors: s.total_errors,
                compaction_count: s.compaction_count,
                last_call_ts_ms: s.last_call_ts_ms,
                entries,
            }
        }))
    }

    pub async fn get_memory_snapshot(&self) -> Result<(String, String, String)> {
        let short_term = self.memory.get_short_term().await.unwrap_or_default();
        let today = crate::time::local_day_string();
        let daily = self.memory.get_daily(&today).await.unwrap_or_default();
        Ok((short_term, daily, today))
    }

    /// Build a context string summarizing recent activity across all sessions.
    /// This gives the background thinker visibility into what's been happening.
    async fn build_background_think_context(&self, own_session_key: &str) -> String {
        let mut ctx = String::new();
        const MAX_EVENTS_PER_SESSION: usize = 15;
        const MAX_CHARS_PER_EVENT: usize = 300;

        // 1. List all session keys
        let all_keys = self.sessions.list_session_keys().await.unwrap_or_default();

        // 2. Current time info
        let now_day = crate::time::local_day_string();
        ctx.push_str(&format!("Current date: {now_day}\n\n"));

        // 3. In-flight sessions
        let in_flight = self.get_in_flight_sessions().await.unwrap_or_default();
        if !in_flight.is_empty() {
            ctx.push_str(&format!(
                "Currently running sessions: {}\n\n",
                in_flight.join(", ")
            ));
        }

        // 4. Summarize recent transcript from each session (excluding own)
        for sk in &all_keys {
            if sk == own_session_key {
                continue;
            }

            let session = match self.sessions.get(sk).await {
                Ok(s) => s,
                Err(_) => continue,
            };

            let events = match self
                .transcripts
                .read_tail(&session.session_id, MAX_EVENTS_PER_SESSION)
                .await
            {
                Ok(e) => e,
                Err(_) => continue,
            };

            if events.is_empty() {
                continue;
            }

            let (stype, label) = classify_session_key(sk);
            ctx.push_str(&format!("### Session: {} ({})\n", label, stype));

            for ev in &events {
                let line = match ev {
                    TranscriptEvent::User(u) => {
                        format!(
                            "  [User] {}",
                            truncate_str_static(&u.body, MAX_CHARS_PER_EVENT)
                        )
                    }
                    TranscriptEvent::Assistant(a) => {
                        format!(
                            "  [Assistant] {}",
                            truncate_str_static(&a.body, MAX_CHARS_PER_EVENT)
                        )
                    }
                    TranscriptEvent::ToolCall(tc) => {
                        format!("  [ToolCall] {} ({})", tc.tool, tc.call_id)
                    }
                    TranscriptEvent::ToolResult(tr) => {
                        let status = if tr.ok { "ok" } else { "FAIL" };
                        let snippet = serde_json::to_string(&tr.result_json).unwrap_or_default();
                        format!(
                            "  [ToolResult {}] {} {}",
                            status,
                            tr.call_id,
                            truncate_str_static(&snippet, 200)
                        )
                    }
                    TranscriptEvent::CompactionSummary(cs) => {
                        format!(
                            "  [Summary] {}",
                            truncate_str_static(&cs.summary, MAX_CHARS_PER_EVENT)
                        )
                    }
                    TranscriptEvent::CustomMessage(cm) => {
                        // Skip system bootstrap messages to keep context concise
                        if cm.body.starts_with("Loaded file:")
                            || cm.body.starts_with("Long-term memory:")
                            || cm.body.starts_with("Daily memory:")
                        {
                            continue;
                        }
                        format!(
                            "  [{}] {}",
                            cm.role,
                            truncate_str_static(&cm.body, MAX_CHARS_PER_EVENT)
                        )
                    }
                    TranscriptEvent::CustomNote(_) => continue,
                };
                ctx.push_str(&line);
                ctx.push('\n');
            }
            ctx.push('\n');
        }

        // 5. Tool session stats
        let tool_stats = self.get_tool_sessions_stats().await.unwrap_or_default();
        if !tool_stats.is_empty() {
            ctx.push_str("### Tool usage stats\n");
            for ts in &tool_stats {
                ctx.push_str(&format!(
                    "  - {}: {} calls, {} errors\n",
                    ts.tool_name, ts.total_calls, ts.total_errors
                ));
            }
            ctx.push('\n');
        }

        if ctx.trim().is_empty() {
            ctx = "No recent activity across sessions.\n".to_string();
        }

        ctx
    }
}

fn truncate_str_static(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out = String::new();
        for (i, ch) in s.chars().enumerate() {
            if i >= max {
                break;
            }
            out.push(ch);
        }
        out.push_str("…");
        out
    }
}

fn classify_session_key(key: &str) -> (String, String) {
    if key == crabbot_shared::DEFAULT_SESSION_KEY {
        return ("user".to_string(), "Default Chat".to_string());
    }
    if key.contains(":background_think") {
        return ("thinking".to_string(), "Background Thinking".to_string());
    }
    if key.starts_with("system:") || key.contains(":task_") {
        let label = key.split(':').last().unwrap_or(key).replace('_', " ");
        return ("task".to_string(), format!("Task: {}", label));
    }
    if key.starts_with("tool:") || key.contains("__tool_") {
        let label = key.split(':').last().unwrap_or(key);
        return ("tool".to_string(), format!("Tool: {}", label));
    }
    ("unknown".to_string(), key.to_string())
}

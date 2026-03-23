use std::sync::Arc;

use crate::{
    agent::AgentRegistry,
    api,
    config::Config,
    error::Result,
    llm,
    memory::service::MemoryService,
    queue, routing,
    run::RunEngine,
    storage::{session_store::SessionStore, transcript_store::TranscriptStore},
    task::manager::TaskManager,
    tools::{registry::ToolRegistry, tool_sessions::ToolSessionStore},
    ui::store::UiHtmlStore,
};

pub async fn create() -> Result<Arc<RunEngine>> {
    // 1) Load config + create directories
    let cfg = Config::load()?;
    cfg.ensure_dirs()?;

    // 2) Wire persistence
    let sessions = Arc::new(SessionStore::open(cfg.paths.sessions_index())?);
    let transcripts = Arc::new(TranscriptStore::open(cfg.paths.transcripts_dir())?);

    // 3) Wire routing + queue + LLM + prompt builder
    let router = Arc::new(routing::router::DefaultSessionRouter::new(
        cfg.routing.clone(),
    ));
    let scheduler = Arc::new(queue::scheduler::QueueScheduler::new(&cfg.queue));

    let memory = Arc::new(MemoryService::open(cfg.paths.memory_dir(), cfg.memory.clone()).await?);
    let llm = llm::create_llm_client(&cfg.llm, &cfg.prompt)?;
    let agents = Arc::new(AgentRegistry::open(cfg.paths.agents_dir()).await?);

    let tasks = Arc::new(TaskManager::open(cfg.paths.tasks_dir(), scheduler.clone()).await?);

    let html_store = UiHtmlStore::new(cfg.paths.ui_dir());

    // 4) Wire per-tool session store (builds up context about tool usage over time)
    let tool_sessions = Arc::new(ToolSessionStore::open(cfg.paths.tool_sessions_dir()).await?);

    // 5) Wire tools
    let tools = Arc::new(ToolRegistry::new(cfg.tool_policy.clone()));
    tools
        .register_builtins(&cfg.paths, &memory, &tasks, &html_store)
        .await?;
    tools.register_optional().await?;

    // 6) Run engine (the "runtime brain")
    let engine = Arc::new(RunEngine::new(
        cfg.clone(),
        router,
        scheduler.clone(),
        sessions,
        transcripts,
        llm,
        tools,
        agents,
        memory,
        tasks,
        html_store,
        tool_sessions,
    ));

    // 7) Start API (HTTP + WS) and hand it the engine handle
    let engine_cp = engine.clone();
    let config_cp = cfg.clone();
    let app = api::http::build_router(engine_cp, config_cp.api.clone())?;
    tokio::spawn(async move {
        let res = api::http::serve(app, config_cp.api.bind).await;
        if let Err(err) = res {
            tracing::error!("API server error: {}", err.to_string());
        }
    });

    Ok(engine)
}

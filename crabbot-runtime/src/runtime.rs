use std::sync::Arc;

use crate::{
    api,
    config::Config,
    error::Result,
    llm,
    memory::{embedder::OllamaOpenAiCompatEmbedder, service::MemoryService},
    prompt, queue, routing,
    run::RunEngine,
    storage::{session_store::SessionStore, transcript_store::TranscriptStore},
    tools::registry::ToolRegistry,
};

pub async fn create() -> Result<Arc<RunEngine>> {
    // 1) Load config + create directories
    let cfg = Config::load()?;
    cfg.ensure_dirs()?;

    // 2) Wire persistence
    let sessions = Arc::new(SessionStore::open(cfg.paths.sessions_index())?);
    let transcripts = Arc::new(TranscriptStore::open(cfg.paths.transcripts_dir())?);

    // 3) Wire tools
    let tools = Arc::new(ToolRegistry::new(cfg.tool_policy.clone()));
    tools.register_builtins().await?;
    tools.register_optional().await?;

    // 4) Wire routing + queue + LLM + prompt builder
    let router = Arc::new(routing::router::DefaultSessionRouter::new(
        cfg.routing.clone(),
    ));
    let scheduler = Arc::new(queue::scheduler::QueueScheduler::new(&cfg.queue));

    let memory = Arc::new(MemoryService::open(cfg.paths.memory_dir(), cfg.memory.clone()).await?);
    let llm = llm::create_llm_client(&cfg.llm, &cfg.prompt)?;

    // 5) Run engine (the “runtime brain”)
    let engine = Arc::new(RunEngine::new(
        cfg.clone(),
        router,
        scheduler.clone(),
        sessions,
        transcripts,
        llm,
        tools,
        memory,
    ));

    // 6) Start API (HTTP + WS) and hand it the engine handle
    // // span in task
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

mod api;
mod cli;
mod config;
mod error;
mod llm;
mod memory;
mod prompt;
mod queue;
mod routing;
mod run;
mod runtime;
mod storage;
mod time;
mod tools;

use crate::error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    crate::error::install_panic_hook();
    crate::error::init_tracing();
    cli::entrypoint().await
}

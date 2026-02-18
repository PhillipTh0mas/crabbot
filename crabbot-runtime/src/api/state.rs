use std::sync::Arc;

use crate::{config::ApiConfig, run::RunEngine};

#[derive(Clone)]
pub struct AppState {
    pub http: Arc<HttpStateInner>,
}

#[derive(Debug)]
pub struct HttpStateInner {
    pub engine: Arc<RunEngine>,
    pub cfg: ApiConfig,
}

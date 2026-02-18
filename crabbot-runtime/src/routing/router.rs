#[derive(Debug)]
pub struct DefaultSessionRouter;
impl DefaultSessionRouter {
    pub fn new(_cfg: crate::config::RoutingConfig) -> Self {
        DefaultSessionRouter {}
    }
}

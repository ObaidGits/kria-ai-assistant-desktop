pub mod auth;
pub mod routes;
pub mod ws;

use axum::Router;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub struct ServerState {
    pub config: kria_core::config::KriaConfig,
}

/// Build the full application router (used by both main and integration tests).
pub fn build_router(state: Arc<ServerState>) -> Router {
    Router::new()
        .merge(routes::api_routes())
        .merge(ws::ws_routes())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

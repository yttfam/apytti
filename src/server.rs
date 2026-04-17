use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::handler::{self, ServerState};

pub fn build_router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route(
            "/api/ask",
            post({
                let state = state.clone();
                move |body| handler::ask(state, body)
            }),
        )
        .route(
            "/health",
            get({
                let state = state.clone();
                move || handler::health(state)
            }),
        )
        .route("/help", get(handler::help))
}

use std::sync::Arc;

use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::handler::{self, ServerState};

pub fn build_router(state: Arc<ServerState>) -> Router {
    let s_ask = state.clone();
    let s_health = state.clone();
    let s_get_cfg = state.clone();
    let s_put_cfg = state.clone();
    let s_get_models = state.clone();
    let s_get_backend_models = state.clone();
    let s_init_models = state.clone();

    Router::new()
        .route(
            "/api/ask",
            post(move |body| handler::ask(s_ask, body)),
        )
        .route("/health", get(move || handler::health(s_health)))
        .route("/help", get(handler::help))
        .route("/config", get(move || handler::get_config(s_get_cfg)))
        .route(
            "/config",
            put(move |headers, body| handler::put_config(s_put_cfg, headers, body)),
        )
        .route("/backends/schema", get(handler::get_backends_schema))
        .route("/models", get(move || handler::get_models(s_get_models)))
        .route(
            "/models/init",
            post(move |headers, query| handler::post_init_models(s_init_models, headers, query)),
        )
        .route(
            "/backends/{name}/models",
            get(move |path| handler::get_backend_models(s_get_backend_models, path)),
        )
        .route(
            "/backends/{name}/projects",
            get(handler::get_backend_projects),
        )
        .route(
            "/backends/{name}/sessions",
            get(handler::get_backend_sessions),
        )
        .route(
            "/backends/{name}/sessions/{sid}",
            delete({
                let state = state.clone();
                move |headers, path| handler::delete_backend_session(state, headers, path)
            }),
        )
        .route(
            "/backends/{name}/sessions/{sid}/status",
            get(handler::get_backend_session_status),
        )
        .route(
            "/backends/{name}/sessions/{sid}/messages",
            get({
                let state = state.clone();
                move |headers, path| {
                    handler::get_backend_session_messages(state, headers, path)
                }
            }),
        )
}

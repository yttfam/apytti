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

    let s_cancel_all = state.clone();
    let s_cancel_session = state.clone();

    Router::new()
        .route(
            "/api/ask",
            post(move |body| handler::ask(s_ask, body))
                .delete(move || handler::cancel_all_ask(s_cancel_all)),
        )
        .route(
            "/backends/{name}/sessions/{sid}/cancel",
            post(move |path| handler::cancel_backend_session(s_cancel_session, path)),
        )
        .route("/health", get(move || handler::health(s_health)))
        .route("/help", get(handler::help))
        .route("/config-ui", get(handler::config_ui))
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
                move |headers, path, query| {
                    handler::get_backend_session_messages(state, headers, path, query)
                }
            }),
        )
        // MCP servers
        .route("/backends/{name}/mcp", get(handler::get_mcp_servers))
        .route(
            "/backends/{name}/mcp",
            post({
                let state = state.clone();
                move |headers, path, body| handler::post_mcp_server(state, headers, path, body)
            }),
        )
        .route(
            "/backends/{name}/mcp/{server}",
            delete({
                let state = state.clone();
                move |headers, path| handler::delete_mcp_server(state, headers, path)
            }),
        )
        // Custom commands
        .route("/backends/{name}/commands", get(handler::get_commands))
        .route(
            "/backends/{name}/commands",
            post({
                let state = state.clone();
                move |headers, path, body| handler::post_command(state, headers, path, body)
            }),
        )
        .route(
            "/backends/{name}/commands/{cmd}",
            get(handler::get_command),
        )
        .route(
            "/backends/{name}/commands/{cmd}",
            delete({
                let state = state.clone();
                move |headers, path| handler::delete_command(state, headers, path)
            }),
        )
        // Agents (read-only)
        .route("/backends/{name}/agents", get(handler::get_agents))
}

//! The two metadata documents of the MCP origin.

use super::{json_response, not_configured, Ctx};
use crate::app::AppState;
use axum::extract::State;
use axum::response::Response;
use serde_json::json;

/// `GET /.well-known/oauth-protected-resource[/mcp]` (RFC 9728): this resource, and the provider that issues tokens
/// for it. Public, so CORS is open.
pub async fn protected_resource(State(state): State<AppState>) -> Response {
    let Some(ctx) = Ctx::new(&state.auth) else {
        return not_configured();
    };
    json_response(
        200,
        json!({
            "resource": ctx.resource(),
            "authorization_servers": [ctx.issuer],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ["openid", "profile", "email", "offline_access"],
            "resource_name": "dibibest",
            "resource_documentation": "https://github.com/codecoradev/dibibest#mcp-for-ai-agents"
        }),
        &[
            ("access-control-allow-origin", "*"),
            ("cache-control", "public, max-age=3600"),
        ],
    )
}

/// `GET /.well-known/oauth-authorization-server[/mcp]` and `/.well-known/openid-configuration[/mcp]`: the
/// provider's own metadata, unchanged. Without it the single-page app answered these paths with HTML and a 200.
pub async fn authorization_server(State(state): State<AppState>) -> Response {
    let Some(ctx) = Ctx::new(&state.auth) else {
        return not_configured();
    };
    match ctx.provider.authorization_server_metadata().await {
        Some(doc) => json_response(
            200,
            doc,
            &[
                ("access-control-allow-origin", "*"),
                ("cache-control", "public, max-age=300"),
            ],
        ),
        None => json_response(
            502,
            json!({ "error": "authorization_server_unavailable" }),
            &[],
        ),
    }
}

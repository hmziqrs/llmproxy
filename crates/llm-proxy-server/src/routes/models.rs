//! `GET /providers/{provider}/v1/models` handler.
//!
//! Returns the merged static and discovered model catalog for a provider.
//!
//! The response is a normalized superset model card that includes both OpenAI
//! and Anthropic fields so either SDK can consume it.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, Response, header};
use axum::response::IntoResponse;
use serde::Serialize;
use tracing::info;

use crate::state::AppState;

use super::error_response::{ClientProtocol, RouteError, route_error_response};

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Top-level response for the models endpoint.
///
/// Follows the OpenAI `/v1/models` response shape as the base, with Anthropic
/// pagination fields added as a superset.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ModelsResponse {
    /// OpenAI-compatible: always `"list"`.
    object: &'static str,
    /// OpenAI-compatible: the list of model cards.
    data: Vec<ModelCard>,
    /// Anthropic-compatible: whether more results exist.
    has_more: bool,
    /// Anthropic-compatible: ID of the first model in the list.
    first_id: Option<String>,
    /// Anthropic-compatible: ID of the last model in the list.
    last_id: Option<String>,
}

/// A normalized model card containing both OpenAI and Anthropic fields.
///
/// Common SDKs should be able to ignore fields they do not use.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ModelCard {
    /// OpenAI: model identifier.
    id: String,
    /// OpenAI: always `"model"`.
    object: &'static str,
    /// OpenAI: Unix timestamp of creation (0 when unknown).
    created: u64,
    /// OpenAI: owner identifier (provider name).
    owned_by: String,
    /// Anthropic: always `"model"`.
    #[serde(rename = "type")]
    model_type: &'static str,
    /// Anthropic: human-readable display name.
    display_name: Option<String>,
    /// Anthropic-compatible creation timestamp, absent when unknown.
    created_at: Option<String>,
    /// The route kinds this model supports (non-standard extension).
    supports: Vec<String>,
    /// Maximum context length in tokens (non-standard extension).
    context_length: Option<u32>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ModelsQuery {
    refresh: Option<String>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `GET /providers/{provider}/v1/models`
///
/// Returns the configured catalog, optionally refreshing discovery with
/// `?refresh=live`. When no catalog is configured, returns an empty list.
pub async fn handle_models(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<ModelsQuery>,
) -> Response<Body> {
    let refresh_live = query.refresh.as_deref() == Some("live");
    match handle_models_inner(&state, &provider, refresh_live).await {
        Ok(response) => response,
        Err(error) => {
            info!(error = %error, "models request failed");
            route_error_response(ClientProtocol::OpenAiChat, error)
        }
    }
}

async fn handle_models_inner(
    state: &AppState,
    provider_name: &str,
    refresh_live: bool,
) -> Result<Response<Body>, RouteError> {
    super::core_pipeline::validate_provider_name(provider_name)?;
    // Look up the provider. Returns 404 if not found.
    let provider = state
        .providers()
        .get(provider_name)
        .ok_or_else(|| RouteError::UnknownProvider(provider_name.to_owned()))?;

    let entries = state
        .model_catalogs()
        .catalog(provider, refresh_live)
        .await
        .map_err(|error| RouteError::Upstream {
            status: axum::http::StatusCode::BAD_GATEWAY,
            body: error.to_string(),
        })?;

    // Build model cards from static entries.
    let data: Vec<ModelCard> = entries
        .iter()
        .map(|entry| ModelCard {
            id: entry.id.clone(),
            object: "model",
            created: 0,
            owned_by: provider.name.clone(),
            model_type: "model",
            display_name: entry.display_name.clone(),
            created_at: None,
            supports: entry
                .supports
                .iter()
                .map(|k| match k {
                    llm_proxy_core::ProviderRouteKind::ChatCompletions => {
                        "chat_completions".to_owned()
                    }
                    llm_proxy_core::ProviderRouteKind::Messages => "messages".to_owned(),
                })
                .collect(),
            context_length: entry.context_length,
        })
        .collect();

    let first_id = data.first().map(|m| m.id.clone());
    let last_id = data.last().map(|m| m.id.clone());

    let response = ModelsResponse {
        object: "list",
        data,
        has_more: false,
        first_id,
        last_id,
    };

    let mut response = (axum::http::StatusCode::OK, axum::Json(response)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    Ok(response)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use llm_proxy_core::{
        AuthStyle, ProviderCatalogConfig, ProviderConfig, ProviderRouteKind, ProviderRoutesConfig,
        StaticModelCatalogEntry,
    };
    use std::collections::HashMap;

    /// Build a minimal AppState with a provider that has a static catalog.
    fn build_state_with_catalog() -> AppState {
        let provider = ProviderConfig {
            name: "test-provider".to_owned(),
            api_key: "sk-test".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: Some(ProviderCatalogConfig {
                mode: llm_proxy_core::ProviderCatalogMode::Static,
                enforce: false,
                cache_ttl: std::time::Duration::from_secs(86400),
                allow: vec!["*".to_owned()],
                deny: vec![],
                models: vec![
                    StaticModelCatalogEntry {
                        id: "deepseek-v3.1".to_owned(),
                        display_name: Some("DeepSeek V3.1".to_owned()),
                        supports: vec![ProviderRouteKind::ChatCompletions],
                        context_length: Some(131072),
                    },
                    StaticModelCatalogEntry {
                        id: "claude-sonnet-4".to_owned(),
                        display_name: Some("Claude Sonnet 4".to_owned()),
                        supports: vec![
                            ProviderRouteKind::ChatCompletions,
                            ProviderRouteKind::Messages,
                        ],
                        context_length: Some(200000),
                    },
                ],
            }),
        };

        let registry =
            llm_proxy_core::ProviderRegistry::from_providers(vec![provider]).expect("registry");
        AppState::new(
            llm_proxy_core::AppConfig {
                server: llm_proxy_core::ServerConfig {
                    bind: "127.0.0.1:3456".parse().unwrap(),
                    request_timeout: std::time::Duration::from_secs(300),
                    log_level: "info".to_owned(),
                    hot_reload: false,
                    server_name: "test".to_owned(),
                },
            },
            registry,
            llm_proxy_provider::ProviderAdapterRegistry::builtin(),
            llm_proxy_provider::ProxyClient::new(),
            crate::state::BuildInfo {
                name: "test",
                version: "0.0.0",
                target: "test",
                git_sha: "test",
            },
        )
    }

    /// Build a minimal AppState with a provider that has NO catalog.
    fn build_state_without_catalog() -> AppState {
        let provider = ProviderConfig {
            name: "no-catalog".to_owned(),
            api_key: "sk-test".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            routes: ProviderRoutesConfig::default(),
            model_aliases: HashMap::new(),
            discovery: None,
            catalog: None,
        };

        let registry =
            llm_proxy_core::ProviderRegistry::from_providers(vec![provider]).expect("registry");
        AppState::new(
            llm_proxy_core::AppConfig {
                server: llm_proxy_core::ServerConfig {
                    bind: "127.0.0.1:3456".parse().unwrap(),
                    request_timeout: std::time::Duration::from_secs(300),
                    log_level: "info".to_owned(),
                    hot_reload: false,
                    server_name: "test".to_owned(),
                },
            },
            registry,
            llm_proxy_provider::ProviderAdapterRegistry::builtin(),
            llm_proxy_provider::ProxyClient::new(),
            crate::state::BuildInfo {
                name: "test",
                version: "0.0.0",
                target: "test",
                git_sha: "test",
            },
        )
    }

    #[tokio::test]
    async fn models_with_catalog_returns_entries() {
        let state = build_state_with_catalog();
        let response = handle_models_inner(&state, "test-provider", false)
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");

        assert_eq!(json["object"], "list");
        assert_eq!(json["data"].as_array().unwrap().len(), 2);
        assert_eq!(json["has_more"], false);

        // Catalog output is sorted by model ID for deterministic responses.
        let m1 = &json["data"][0];
        assert_eq!(m1["id"], "claude-sonnet-4");
        assert_eq!(m1["object"], "model");
        assert_eq!(m1["owned_by"], "test-provider");
        assert_eq!(m1["display_name"], "Claude Sonnet 4");
        assert_eq!(m1["context_length"], 200000);

        // Second model
        let m2 = &json["data"][1];
        assert_eq!(m2["id"], "deepseek-v3.1");
        assert_eq!(m2["display_name"], "DeepSeek V3.1");
        assert_eq!(m2["context_length"], 131072);

        // Pagination fields
        assert_eq!(json["first_id"], "claude-sonnet-4");
        assert_eq!(json["last_id"], "deepseek-v3.1");
    }

    #[tokio::test]
    async fn models_without_catalog_returns_empty_list() {
        let state = build_state_without_catalog();
        let response = handle_models_inner(&state, "no-catalog", false)
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");

        assert_eq!(json["object"], "list");
        assert_eq!(json["data"].as_array().unwrap().len(), 0);
        assert_eq!(json["has_more"], false);
        assert!(json["first_id"].is_null());
        assert!(json["last_id"].is_null());
    }

    #[tokio::test]
    async fn models_unknown_provider_returns_404() {
        let state = build_state_with_catalog();
        let result = handle_models_inner(&state, "nonexistent", false).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RouteError::UnknownProvider(_)));
    }

    #[tokio::test]
    async fn model_card_has_no_credentials() {
        let state = build_state_with_catalog();
        let response = handle_models_inner(&state, "test-provider", false)
            .await
            .expect("response");

        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .expect("body");
        let body_str = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(
            !body_str.contains("sk-test"),
            "model endpoint output must not contain credentials, got: {body_str}"
        );
    }

    #[test]
    fn source_guard_models_uses_current_architecture() {
        let source = include_str!("models.rs");
        let prod = source
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or(source);

        assert!(!prod.contains("ApiError"), "models.rs must use RouteError");
        assert!(
            !prod.contains("crate::error::"),
            "models.rs must not import from crate::error"
        );
    }
}

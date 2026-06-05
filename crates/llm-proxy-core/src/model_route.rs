//! Model routing: resolve a client-facing model name to a provider target.
//!
//! The router only selects *where* a request goes. It does not translate
//! protocol fields — that is the job of the adapter layer.

use std::collections::HashMap;

use crate::provider_config::ModelRoute;

// ---------------------------------------------------------------------------
// ProviderTarget
// ---------------------------------------------------------------------------

/// Resolved routing target: which provider and which upstream model to use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderTarget {
    /// Provider name (identifies a provider config file).
    pub provider: String,
    /// The model name the client originally requested.
    pub requested_model: String,
    /// The model name to send to the upstream provider.
    /// May differ from `requested_model` when an alias is configured.
    pub upstream_model: String,
}

// ---------------------------------------------------------------------------
// ModelRouteError
// ---------------------------------------------------------------------------

/// Errors produced during model route resolution.
///
/// This enum is `#[non_exhaustive]` to allow adding new error variants in
/// future phases without breaking downstream `match` expressions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelRouteError {
    /// The requested model name is not present in the routing table.
    #[error("unknown model: {0}")]
    UnknownModel(String),
}

// ---------------------------------------------------------------------------
// resolve_model_route
// ---------------------------------------------------------------------------

/// Resolve a client-facing model name to a [`ProviderTarget`].
///
/// Looks up the model name in the routing table. If found, returns the
/// provider name and effective upstream model name. If `upstream_model` is
/// not specified in the route, the requested model name is used as-is.
///
/// # Errors
///
/// Returns [`ModelRouteError::UnknownModel`] if the model name is not in
/// the routing table.
pub fn resolve_model_route(
    routes: &HashMap<String, ModelRoute>,
    requested_model: &str,
) -> Result<ProviderTarget, ModelRouteError> {
    let route = routes
        .get(requested_model)
        .ok_or_else(|| ModelRouteError::UnknownModel(requested_model.to_owned()))?;

    Ok(ProviderTarget {
        provider: route.provider.clone(),
        requested_model: requested_model.to_owned(),
        upstream_model: route
            .upstream_model
            .clone()
            .unwrap_or_else(|| requested_model.to_owned()),
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_routes() -> HashMap<String, ModelRoute> {
        let mut m = HashMap::new();
        m.insert(
            "kimi-k2.6".to_owned(),
            ModelRoute {
                provider: "opencode-go".to_owned(),
                upstream_model: None,
            },
        );
        m.insert(
            "claude-4".to_owned(),
            ModelRoute {
                provider: "opencode-zen".to_owned(),
                upstream_model: Some("claude-sonnet-4-20250514".to_owned()),
            },
        );
        m
    }

    // -- Upstream model alias resolves correctly --------------------------------

    #[test]
    fn upstream_model_alias_resolves_correctly() {
        let routes = make_routes();
        let target = resolve_model_route(&routes, "claude-4").expect("resolve");
        assert_eq!(target.provider, "opencode-zen");
        assert_eq!(target.requested_model, "claude-4");
        assert_eq!(target.upstream_model, "claude-sonnet-4-20250514");
    }

    // -- Unknown model returns UnknownModel -------------------------------------

    #[test]
    fn unknown_model_returns_error() {
        let routes = make_routes();
        let result = resolve_model_route(&routes, "nonexistent-model");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ModelRouteError::UnknownModel(ref m) if m == "nonexistent-model"),
            "expected UnknownModel error, got: {err}"
        );
    }

    // -- Model without upstream_model uses requested name -----------------------

    #[test]
    fn model_without_upstream_model_uses_requested_name() {
        let routes = make_routes();
        let target = resolve_model_route(&routes, "kimi-k2.6").expect("resolve");
        assert_eq!(target.provider, "opencode-go");
        assert_eq!(target.requested_model, "kimi-k2.6");
        assert_eq!(target.upstream_model, "kimi-k2.6");
    }

    // -- Empty routes table returns UnknownModel --------------------------------

    #[test]
    fn empty_routes_returns_unknown_model() {
        let routes: HashMap<String, ModelRoute> = HashMap::new();
        let result = resolve_model_route(&routes, "anything");
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ModelRouteError::UnknownModel(ref m) if m == "anything")
        );
    }

    // -- ProviderTarget equality ------------------------------------------------

    #[test]
    fn provider_target_equality() {
        let a = ProviderTarget {
            provider: "test".to_owned(),
            requested_model: "model".to_owned(),
            upstream_model: "model".to_owned(),
        };
        let b = ProviderTarget {
            provider: "test".to_owned(),
            requested_model: "model".to_owned(),
            upstream_model: "model".to_owned(),
        };
        assert_eq!(a, b);
    }

    // -- ModelRouteError display ------------------------------------------------

    #[test]
    fn model_route_error_display() {
        let err = ModelRouteError::UnknownModel("gpt-99".to_owned());
        let msg = err.to_string();
        assert!(msg.contains("unknown model"), "expected 'unknown model', got: {msg}");
        assert!(msg.contains("gpt-99"), "expected 'gpt-99', got: {msg}");
    }

    // -- Multiple routes resolve independently ----------------------------------

    #[test]
    fn multiple_routes_resolve_independently() {
        let routes = make_routes();
        let t1 = resolve_model_route(&routes, "kimi-k2.6").expect("resolve kimi");
        let t2 = resolve_model_route(&routes, "claude-4").expect("resolve claude");
        assert_ne!(t1.provider, t2.provider);
        assert_ne!(t1.upstream_model, t2.upstream_model);
    }
}

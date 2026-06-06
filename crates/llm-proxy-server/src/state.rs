use std::sync::Arc;
use std::time::Duration;

use llm_proxy_core::{AppConfig, Counter, Metrics, ProviderRegistry};
use llm_proxy_provider::{ProviderAdapterRegistry, ProxyClient};

use crate::middleware::{RateLimiter, RequestDeduplicator, RequestIdGenerator};

/// Default rate limit (requests per minute) when no config override is provided.
///
/// TODO: Make this configurable via TOML config in a future phase.
const DEFAULT_RATE_LIMIT_RPM: u32 = 100;

/// Static information about this binary.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// Package name.
    pub name: &'static str,
    /// Package version.
    pub version: &'static str,
    /// Target triple the binary was compiled for.
    pub target: &'static str,
    /// Git commit SHA the binary was built from.
    pub git_sha: &'static str,
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Application state shared with every handler.
///
/// Cheap to clone: all fields are `Arc`-wrapped so cloning only bumps
/// reference counts. Required by axum's `State` extractor.
///
/// # Security note
///
/// Manual `Debug` impl is provided to ensure `api_key` in nested config types
/// is never leaked through debug formatting (e.g. in error logs).
///
/// The transitive redaction chain for Debug is:
/// - `AppState` -> `app_config` field: `AppConfig` has no secrets (safe).
/// - `AppState` -> `providers` field: `ProviderRegistry` -> `ProviderConfig`
///   (manual Debug, redacts api_key). Safe.
/// - `AppState` -> `provider_adapters` field: `ProviderAdapterRegistry` holds
///   only `HashMap<ProviderProtocol, ProviderAdapter>` -- no secrets. Safe.
/// - `AppState` -> remaining fields: `BuildInfo`, `Counter`, `Metrics`,
///   `RateLimiter`, `RequestDeduplicator`, `RequestIdGenerator` -- none hold
///   secrets. Safe.
///
/// **Maintenance note:** If any of the above types gains a plain derived `Debug`
/// that contains secrets, the redaction chain breaks silently. The test
/// `app_state_debug_does_not_leak_api_key` provides regression coverage.
#[derive(Clone)]
#[non_exhaustive]
pub struct AppState {
    /// TOML application config.
    pub(crate) app_config: Arc<AppConfig>,
    /// Provider registry loaded from TOML provider files.
    pub(crate) providers: Arc<ProviderRegistry>,
    /// Provider adapter registry (always present).
    pub(crate) provider_adapters: Arc<ProviderAdapterRegistry>,
    /// Protocol-neutral HTTP transport client.
    pub(crate) proxy_client: Arc<ProxyClient>,
    /// Build identifier reported by `/version`.
    pub(crate) build: Arc<BuildInfo>,
    /// Token counter for estimating token usage.
    pub(crate) token_counter: Arc<Counter>,
    /// Runtime metrics collector.
    pub(crate) metrics: Arc<Metrics>,
    /// Per-IP rate limiter.
    pub(crate) rate_limiter: Arc<RateLimiter>,
    /// Request deduplicator for idempotent request handling.
    pub(crate) request_dedup: Arc<RequestDeduplicator>,
    /// Request ID generator.
    pub(crate) request_id_gen: Arc<RequestIdGenerator>,
}

impl std::fmt::Debug for AppState {
    // SECURITY: This manual Debug impl delegates through Arc to each field's
    // Debug. The redaction chain documented on the struct must be maintained.
    // The test `app_state_debug_does_not_leak_api_key` provides regression
    // coverage. See struct-level doc comment for the full chain.
    //
    // NOTE: A `redacted_debug!()` macro could reduce boilerplate for types
    // with sensitive fields, but would add a procedural-macro dependency.
    // The manual impl is preferred for now because: (a) AppState has few
    // fields, (b) the redaction chain is explicitly documented and tested,
    // and (c) a macro would obscure the security-critical delegation paths.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("app_config", &self.app_config)
            .field("providers", &self.providers)
            .field("provider_adapters", &self.provider_adapters)
            .field("proxy_client", &self.proxy_client)
            .field("build", &self.build)
            .field("token_counter", &self.token_counter)
            .field("metrics", &self.metrics)
            .field("rate_limiter", &self.rate_limiter)
            .field("request_dedup", &self.request_dedup)
            .field("request_id_gen", &self.request_id_gen)
            .finish()
    }
}

impl AppState {
    /// Construct `AppState` with TOML configuration.
    #[must_use]
    pub fn new(
        app_config: AppConfig,
        providers: ProviderRegistry,
        provider_adapters: ProviderAdapterRegistry,
        proxy_client: ProxyClient,
        build: BuildInfo,
    ) -> Self {
        Self {
            app_config: Arc::new(app_config),
            providers: Arc::new(providers),
            provider_adapters: Arc::new(provider_adapters),
            proxy_client: Arc::new(proxy_client),
            build: Arc::new(build),
            token_counter: Arc::new(Counter::new()),
            metrics: Arc::new(Metrics::new()),
            rate_limiter: Arc::new(RateLimiter::new(DEFAULT_RATE_LIMIT_RPM)),
            request_dedup: Arc::new(RequestDeduplicator::new()),
            request_id_gen: Arc::new(RequestIdGenerator::new()),
        }
    }

    /// Request timeout duration from app config.
    pub fn request_timeout(&self) -> Duration {
        self.app_config.server.request_timeout
    }

    /// Server name for responses and version endpoints.
    pub fn server_name(&self) -> &str {
        &self.app_config.server.server_name
    }

    /// Access the TOML app config.
    pub fn app_config(&self) -> &AppConfig {
        &self.app_config
    }

    /// Access the provider registry.
    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    /// Return the bind address the server should listen on.
    pub fn bind_address(&self) -> std::net::SocketAddr {
        self.app_config.server.bind
    }

    /// Access build metadata (version, target, git SHA).
    pub fn build_info(&self) -> &BuildInfo {
        &self.build
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::SocketAddr;

    /// Helper: build a minimal TOML AppConfig for tests.
    fn make_app_config() -> AppConfig {
        use llm_proxy_core::ServerConfig;
        AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:3456".parse().unwrap(),
                request_timeout: Duration::from_secs(300),
                log_level: "info".to_owned(),
                hot_reload: false,
                server_name: "test-proxy".to_owned(),
            },
            models: HashMap::new(),
        }
    }

    /// Helper: build a minimal ProviderRegistry for tests.
    fn make_provider_registry() -> ProviderRegistry {
        ProviderRegistry::from_providers(vec![]).expect("empty registry")
    }

    fn make_build_info() -> BuildInfo {
        BuildInfo {
            name: "test",
            version: "0.0.0",
            target: "test",
            git_sha: "test",
        }
    }

    // -- AppState construction ------------------------------------------------

    #[test]
    fn app_state_construction() {
        let state = AppState::new(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), "test-proxy");
        assert_eq!(state.request_timeout(), Duration::from_secs(300));
    }

    // -- Helper methods -------------------------------------------------------

    #[test]
    fn helpers_return_app_config_values() {
        let state = AppState::new(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), "test-proxy");
        assert_eq!(state.request_timeout(), Duration::from_secs(300));
    }

    // -- Debug does not leak API keys -----------------------------------------

    #[test]
    fn app_state_debug_does_not_leak_api_key() {
        use llm_proxy_core::{AuthStyle, ProviderConfig};
        let provider = ProviderConfig {
            name: "test".to_owned(),
            api_key: "sk-secret-key-99999".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: HashMap::new(),
            models: HashMap::new(),
        };
        let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
        let state = AppState::new(
            make_app_config(),
            registry,
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        let debug_output = format!("{:?}", state);
        assert!(
            !debug_output.contains("sk-secret-key-99999"),
            "Debug output must not contain the actual api_key, got: {debug_output}"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output must show [REDACTED] for api_key, got: {debug_output}"
        );
    }

    #[test]
    fn app_state_debug_covers_all_fields() {
        // Regression test: verify that the Debug impl actually outputs all
        // field names. If a field is added to AppState but missing from the
        // manual Debug impl, this test catches it.
        let state = AppState::new(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        let debug_output = format!("{:?}", state);
        // All fields that should appear in the Debug output.
        for field in [
            "app_config",
            "providers",
            "provider_adapters",
            "proxy_client",
            "build",
            "token_counter",
            "metrics",
            "rate_limiter",
            "request_dedup",
            "request_id_gen",
        ] {
            assert!(
                debug_output.contains(field),
                "Debug output must contain field '{field}', got: {debug_output}"
            );
        }
    }

    // -- Provider adapters are populated --------------------------------------

    #[test]
    fn provider_adapters_are_populated() {
        let state = AppState::new(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        // Use >= 4 rather than == 4 so adding new builtin adapters does not
        // break this test (the intent is to verify adapters are populated).
        let protocol_names = state.provider_adapters.protocol_names();
        assert!(
            protocol_names.len() >= 4,
            "expected at least 4 builtin adapters, got {len}",
            len = protocol_names.len()
        );
        // Verify specific known protocol names are present for stronger coverage.
        assert!(
            protocol_names.contains(&"openai_chat_completions"),
            "missing openai_chat_completions adapter"
        );
        assert!(
            protocol_names.contains(&"anthropic_messages"),
            "missing anthropic_messages adapter"
        );
    }

    // -- TOML provider protocol validation against builtin -------------------

    #[test]
    fn toml_provider_protocol_validation_against_builtin() {
        use llm_proxy_core::{
            AuthStyle, ProviderAdapterConfig, ProviderConfig, ProviderModelConfig,
        };
        let provider = ProviderConfig {
            name: "test".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com/v1".to_owned(),
                    },
                );
                m.insert(
                    "messages".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "anthropic_messages".to_owned(),
                        endpoint: "https://example.com/v1/messages".to_owned(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "gpt-5.4".to_owned(),
                    ProviderModelConfig {
                        adapter: "chat".to_owned(),
                    },
                );
                m
            },
        };
        let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
        let adapter_reg = ProviderAdapterRegistry::builtin();
        // Validate protocols against builtins
        let result = registry.validate_protocols(adapter_reg.protocol_names());
        assert!(result.is_ok(), "all protocols are builtin, got: {result:?}");
    }

    // -- TOML unknown protocol fails validation against builtin ---------------

    #[test]
    fn toml_unknown_protocol_fails_validation_against_builtin() {
        use llm_proxy_core::{AuthStyle, ProviderAdapterConfig, ProviderConfig};
        let provider = ProviderConfig {
            name: "bad".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "fake".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "not_a_real_protocol".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                    },
                );
                m
            },
            models: HashMap::new(),
        };
        let registry = ProviderRegistry::from_providers(vec![provider]).expect("registry");
        let adapter_reg = ProviderAdapterRegistry::builtin();
        let result = registry.validate_protocols(adapter_reg.protocol_names());
        assert!(result.is_err(), "unknown protocol should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown protocol"),
            "expected unknown protocol error, got: {err}"
        );
    }

    // -- Config extension dispatch logic -------------------------------------

    #[test]
    fn config_extension_dispatch_classifies_correctly() {
        /// Mimics the match arm logic from cmd_serve to verify extension
        /// classification without requiring a full integration test.
        fn classify(path: &std::path::Path) -> &'static str {
            match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
                "toml" => "toml",
                "json" => "json",
                _ => "unsupported",
            }
        }

        assert_eq!(classify(std::path::Path::new("config.toml")), "toml");
        assert_eq!(classify(std::path::Path::new("config.json")), "json");
        assert_eq!(classify(std::path::Path::new("config.yaml")), "unsupported");
        assert_eq!(classify(std::path::Path::new("config.txt")), "unsupported");
        assert_eq!(classify(std::path::Path::new("config")), "unsupported");
        assert_eq!(classify(std::path::Path::new("config.YAML")), "unsupported");
    }

    // -- Send + Sync static assertion for AppState ---------------------------

    #[test]
    fn app_state_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<AppState>();
    }

    // -- bind_address() tests ------------------------------------------------

    #[test]
    fn bind_address_returns_configured_bind() {
        let mut cfg = make_app_config();
        cfg.server.bind = "10.0.0.1:8080".parse().unwrap();
        let state = AppState::new(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(
            state.bind_address(),
            "10.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
    }

    // -- Clone shares Arc references -----------------------------------------

    #[test]
    fn clone_shares_arc_references() {
        let state = AppState::new(
            make_app_config(),
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        let cloned = state.clone();
        assert!(
            Arc::ptr_eq(&state.metrics, &cloned.metrics),
            "cloned AppState should share the same Arc<Metrics>"
        );
        assert!(
            Arc::ptr_eq(&state.token_counter, &cloned.token_counter),
            "cloned AppState should share the same Arc<Counter>"
        );
        assert!(
            Arc::ptr_eq(&state.build, &cloned.build),
            "cloned AppState should share the same Arc<BuildInfo>"
        );
    }

    // -- Multi-provider TOML validation --------------------------------------

    #[test]
    fn multi_provider_toml_mixed_protocols() {
        use llm_proxy_core::{AuthStyle, ProviderAdapterConfig, ProviderConfig};

        // Provider with valid protocols
        let good = ProviderConfig {
            name: "good".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "chat".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "openai_chat_completions".to_owned(),
                        endpoint: "https://example.com/v1".to_owned(),
                    },
                );
                m
            },
            models: HashMap::new(),
        };

        // Provider with invalid protocol
        let bad = ProviderConfig {
            name: "bad".to_owned(),
            api_key: "key".to_owned(),
            auth_style: AuthStyle::Bearer,
            adapters: {
                let mut m = HashMap::new();
                m.insert(
                    "fake".to_owned(),
                    ProviderAdapterConfig {
                        protocol: "not_real".to_owned(),
                        endpoint: "https://example.com".to_owned(),
                    },
                );
                m
            },
            models: HashMap::new(),
        };

        let registry = ProviderRegistry::from_providers(vec![good, bad]).expect("registry");
        let adapter_reg = ProviderAdapterRegistry::builtin();
        let result = registry.validate_protocols(adapter_reg.protocol_names());
        assert!(
            result.is_err(),
            "mixed providers with one bad protocol should fail"
        );
    }

    // -- Boundary values for helpers -----------------------------------------

    #[test]
    fn zero_duration_timeout_is_returned() {
        let mut cfg = make_app_config();
        cfg.server.request_timeout = Duration::ZERO;
        let state = AppState::new(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.request_timeout(), Duration::ZERO);
    }

    #[test]
    fn empty_server_name_is_returned() {
        let mut cfg = make_app_config();
        cfg.server.server_name = String::new();
        let state = AppState::new(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), "");
    }

    // -- Unicode server_name ---------------------------------------------------

    #[test]
    fn unicode_server_name_is_returned_verbatim() {
        let mut cfg = make_app_config();
        cfg.server.server_name = "代理服务器-ünïcödé".to_owned();
        let state = AppState::new(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), "代理服务器-ünïcödé");
    }

    #[test]
    fn very_long_server_name_is_returned() {
        let mut cfg = make_app_config();
        let long_name = "x".repeat(10_000);
        cfg.server.server_name = long_name.clone();
        let state = AppState::new(
            cfg,
            make_provider_registry(),
            ProviderAdapterRegistry::builtin(),
            ProxyClient::new(),
            make_build_info(),
        );
        assert_eq!(state.server_name(), long_name);
    }
}

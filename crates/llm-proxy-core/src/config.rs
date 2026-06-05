//! Configuration loading and types for `llm-proxy-core`.
//!
//! Mirrors the Go `oc-go-cc` config system: JSON config files with
//! `${ENV_VAR}` interpolation, environment variable overrides, defaults,
//! and validation.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Defaults (match oc-go-cc internal/config/loader.go constants)
// ---------------------------------------------------------------------------

/// Default bind address used by the original Rust config.
const DEFAULT_BIND: &str = "0.0.0.0:8080";
/// Default request timeout used by the original Rust config.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 3456;
const DEFAULT_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const DEFAULT_GO_ANTHROPIC_BASE_URL: &str = "https://opencode.ai/zen/go/v1/messages";
const DEFAULT_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_LOG_LEVEL: &str = "info";

const DEFAULT_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1/chat/completions";
const DEFAULT_ZEN_ANTHROPIC_BASE_URL: &str = "https://opencode.ai/zen/v1/messages";
const DEFAULT_ZEN_RESPONSES_BASE_URL: &str = "https://opencode.ai/zen/v1/responses";
const DEFAULT_ZEN_GEMINI_BASE_URL: &str = "https://opencode.ai/zen/v1/models";

// ---------------------------------------------------------------------------
// Config path helpers
// ---------------------------------------------------------------------------

/// Resolve the config file path.
///
/// Priority:
/// 1. `OC_GO_CC_CONFIG` env var (explicit override)
/// 2. `~/.config/oc-go-cc/config.json` (default)
///
/// # Trust boundary
///
/// The `OC_GO_CC_CONFIG` env var accepts arbitrary filesystem paths without
/// sanitization. This is standard practice for env-var-driven config (the
/// process owner controls env vars), but a compromised env var could direct
/// the proxy to read an attacker-controlled file. The config file content is
/// treated as trusted input (JSON with env-var interpolation). Deployments
/// should ensure only privileged users can set this env var.
fn resolve_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("OC_GO_CC_CONFIG") {
        return PathBuf::from(path);
    }
    expand_home("~/.config/oc-go-cc/config.json")
}

/// Replace a leading `~` with the user's home directory.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

// ---------------------------------------------------------------------------
// Env-var interpolation
// ---------------------------------------------------------------------------

/// Replace `${ENV_VAR}` patterns in `input` with the value of the
/// corresponding environment variable.  Unset variables are left as-is.
fn interpolate_env_vars(input: &str) -> String {
    // Compile the regex once rather than on every call.
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\$\{([A-Za-z0-9_]+)\}").expect("env var regex is valid"));
    re.replace_all(input, |caps: &regex::Captures<'_>| {
        let var_name = &caps[1];
        std::env::var(var_name).unwrap_or_else(|_| caps[0].to_owned())
    })
    .into_owned()
}

// ---------------------------------------------------------------------------
// Top-level Config
// ---------------------------------------------------------------------------

/// Top-level server configuration.
///
/// Fully compatible with the Go `oc-go-cc` JSON schema while also
/// retaining the original Rust fields (`bind`, `request_timeout`,
/// `server_name`) for backward compatibility.
///
/// # Security note
///
/// The `api_key` field is redacted in `Debug` output and excluded from
/// `Serialize` output to prevent accidental credential leakage in logs,
/// error messages, or API responses. Config files are loaded from trusted
/// input only.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    // -- oc-go-cc fields ---------------------------------------------------
    /// API key used to authenticate with upstream providers.
    ///
    /// Redacted in Debug output. Not serialized by `serde_json::to_string`
    /// (use explicit methods if you need to write a config file).
    #[serde(default, skip_serializing)]
    pub api_key: String,
    /// Host interface to bind the proxy to.
    #[serde(default)]
    pub host: String,
    /// Port the proxy listens on.
    #[serde(default)]
    pub port: u16,
    /// Enable hot-reload of the config file at runtime.
    #[serde(default)]
    pub hot_reload: bool,
    /// Enable scenario-based routing for streaming requests.
    #[serde(default)]
    pub enable_streaming_scenario_routing: bool,
    /// When `true` the proxy will honour the model explicitly requested
    /// by the client instead of applying scenario-based routing.
    #[serde(default)]
    pub respect_requested_model: bool,
    /// Named model routing rules.  The key is a scenario name such as
    /// `"default"`, `"think"`, `"complex"`, etc.
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
    /// Fallback models per scenario.  Tried in order when the primary
    /// model for a scenario fails.
    #[serde(default)]
    pub fallbacks: HashMap<String, Vec<ModelConfig>>,
    /// Upstream OpenCode Go API settings.
    #[serde(default)]
    pub opencode_go: OpenCodeGoConfig,
    /// Upstream OpenCode Zen API settings.
    #[serde(default)]
    pub opencode_zen: OpenCodeZenConfig,
    /// Logging configuration.
    #[serde(default)]
    pub logging: LoggingConfig,

    // -- legacy Rust fields ------------------------------------------------
    /// Address the HTTP server binds to (legacy).
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    /// Maximum request duration before timeout.  Accepts humantime
    /// strings in TOML, e.g. `request_timeout = "60s"` (legacy).
    #[serde(default = "default_request_timeout", with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Public server name reported by `/version` (legacy).
    #[serde(default = "default_server_name")]
    pub server_name: String,
}

fn default_bind() -> SocketAddr {
    DEFAULT_BIND.parse().expect("default bind address is valid")
}

fn default_request_timeout() -> Duration {
    Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS)
}

fn default_server_name() -> String {
    env!("CARGO_PKG_NAME").to_owned()
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("api_key", &"[REDACTED]")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("hot_reload", &self.hot_reload)
            .field("enable_streaming_scenario_routing", &self.enable_streaming_scenario_routing)
            .field("respect_requested_model", &self.respect_requested_model)
            .field("models", &self.models)
            .field("fallbacks", &self.fallbacks)
            .field("opencode_go", &self.opencode_go)
            .field("opencode_zen", &self.opencode_zen)
            .field("logging", &self.logging)
            .field("bind", &self.bind)
            .field("request_timeout", &self.request_timeout)
            .field("server_name", &self.server_name)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ModelConfig
// ---------------------------------------------------------------------------

/// Routing rules for a specific model / scenario.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// Provider identifier (e.g. `"opencode-go"`).
    #[serde(default)]
    pub provider: String,
    /// Model ID to request from the upstream provider.
    #[serde(default)]
    pub model_id: String,
    /// Sampling temperature.
    #[serde(default)]
    pub temperature: f64,
    /// Maximum tokens the model may generate.
    #[serde(default)]
    pub max_tokens: i64,
    /// Token count above which this scenario is selected.
    #[serde(default)]
    pub context_threshold: i64,
    /// Reasoning effort hint (e.g. `"max"`).
    #[serde(default)]
    pub reasoning_effort: String,
    /// Arbitrary thinking configuration forwarded to the model.
    #[serde(default)]
    pub thinking: Option<Value>,
}

// ---------------------------------------------------------------------------
// OpenCode sub-configs
// ---------------------------------------------------------------------------

/// Upstream OpenCode Go API settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeGoConfig {
    /// Base URL for the OpenAI-compatible endpoint.
    #[serde(default)]
    pub base_url: String,
    /// Base URL for the Anthropic-compatible endpoint.
    #[serde(default)]
    pub anthropic_base_url: String,
    /// Request timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: u64,
}

/// Upstream OpenCode Zen API settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeZenConfig {
    /// Base URL for the OpenAI-compatible endpoint.
    #[serde(default)]
    pub base_url: String,
    /// Base URL for the Anthropic-compatible endpoint.
    #[serde(default)]
    pub anthropic_base_url: String,
    /// Base URL for the Responses API endpoint.
    #[serde(default)]
    pub responses_base_url: String,
    /// Base URL for the Gemini-compatible endpoint.
    #[serde(default)]
    pub gemini_base_url: String,
    /// Request timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: u64,
}

// ---------------------------------------------------------------------------
// LoggingConfig
// ---------------------------------------------------------------------------

/// Logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// Log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`.
    #[serde(default)]
    pub level: String,
    /// Whether to log every incoming request.
    #[serde(default)]
    pub requests: bool,
}

// ---------------------------------------------------------------------------
// Defaults for sub-structs
// ---------------------------------------------------------------------------

impl Default for OpenCodeGoConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_GO_BASE_URL.to_owned(),
            anthropic_base_url: DEFAULT_GO_ANTHROPIC_BASE_URL.to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl Default for OpenCodeZenConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_ZEN_BASE_URL.to_owned(),
            anthropic_base_url: DEFAULT_ZEN_ANTHROPIC_BASE_URL.to_owned(),
            responses_base_url: DEFAULT_ZEN_RESPONSES_BASE_URL.to_owned(),
            gemini_base_url: DEFAULT_ZEN_GEMINI_BASE_URL.to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: DEFAULT_LOG_LEVEL.to_owned(),
            requests: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Config impl
// ---------------------------------------------------------------------------

impl Config {
    /// Load configuration from the default path.
    ///
    /// Resolution order matches `oc-go-cc`:
    /// 1. `OC_GO_CC_CONFIG` env var
    /// 2. `~/.config/oc-go-cc/config.json`
    pub fn load_default() -> Result<Self, crate::error::CoreError> {
        let path = resolve_config_path();
        Self::load(&path)
    }

    /// Load configuration from a JSON file.
    ///
    /// The raw file content is first processed for `${ENV_VAR}`
    /// interpolation, then parsed as JSON.  Environment variable
    /// overrides are applied next, followed by defaults and validation.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, crate::error::CoreError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|source| {
            crate::error::CoreError::ConfigLoad {
                path: path.to_path_buf(),
                source,
            }
        })?;

        // Interpolate ${ENV_VAR} patterns.
        let interpolated = interpolate_env_vars(&raw);

        // Parse JSON.
        let mut cfg: Self = serde_json::from_str(&interpolated)
            .map_err(crate::error::CoreError::ConfigParseJson)?;

        apply_env_overrides(&mut cfg);
        apply_defaults(&mut cfg);
        validate(&cfg, path)?;

        Ok(cfg)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // oc-go-cc fields
            api_key: String::new(),
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            hot_reload: false,
            enable_streaming_scenario_routing: false,
            respect_requested_model: false,
            models: HashMap::new(),
            fallbacks: HashMap::new(),
            opencode_go: OpenCodeGoConfig::default(),
            opencode_zen: OpenCodeZenConfig::default(),
            logging: LoggingConfig::default(),

            // legacy Rust fields
            bind: default_bind(),
            request_timeout: default_request_timeout(),
            server_name: default_server_name(),
        }
    }
}

// ---------------------------------------------------------------------------
// Env overrides
// ---------------------------------------------------------------------------

/// Apply well-known environment variable overrides to the config.
fn apply_env_overrides(cfg: &mut Config) {
    if let Ok(v) = std::env::var("OC_GO_CC_API_KEY") {
        cfg.api_key = v;
    }
    if let Ok(v) = std::env::var("OC_GO_CC_HOST") {
        cfg.host = v;
    }
    if let Ok(v) = std::env::var("OC_GO_CC_PORT") {
        if let Ok(port) = v.parse::<u16>() {
            cfg.port = port;
        }
    }
    if let Ok(v) = std::env::var("OC_GO_CC_OPENCODE_URL") {
        cfg.opencode_go.base_url = v;
    }
    if let Ok(v) = std::env::var("OC_GO_CC_OPENCODE_ZEN_URL") {
        cfg.opencode_zen.base_url = v;
    }
    if let Ok(v) = std::env::var("OC_GO_CC_LOG_LEVEL") {
        cfg.logging.level = v;
    }
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Fill in missing values with sensible defaults (mirrors Go `applyDefaults`).
fn apply_defaults(cfg: &mut Config) {
    if cfg.host.is_empty() {
        cfg.host = DEFAULT_HOST.to_owned();
    }
    if cfg.port == 0 {
        cfg.port = DEFAULT_PORT;
    }

    // opencode_go defaults
    if cfg.opencode_go.base_url.is_empty() {
        cfg.opencode_go.base_url = DEFAULT_GO_BASE_URL.to_owned();
    }
    if cfg.opencode_go.anthropic_base_url.is_empty() {
        cfg.opencode_go.anthropic_base_url = DEFAULT_GO_ANTHROPIC_BASE_URL.to_owned();
    }
    if cfg.opencode_go.timeout_ms == 0 {
        cfg.opencode_go.timeout_ms = DEFAULT_TIMEOUT_MS;
    }

    // opencode_zen defaults
    if cfg.opencode_zen.base_url.is_empty() {
        cfg.opencode_zen.base_url = DEFAULT_ZEN_BASE_URL.to_owned();
    }
    if cfg.opencode_zen.anthropic_base_url.is_empty() {
        cfg.opencode_zen.anthropic_base_url = DEFAULT_ZEN_ANTHROPIC_BASE_URL.to_owned();
    }
    if cfg.opencode_zen.responses_base_url.is_empty() {
        cfg.opencode_zen.responses_base_url = DEFAULT_ZEN_RESPONSES_BASE_URL.to_owned();
    }
    if cfg.opencode_zen.gemini_base_url.is_empty() {
        cfg.opencode_zen.gemini_base_url = DEFAULT_ZEN_GEMINI_BASE_URL.to_owned();
    }
    if cfg.opencode_zen.timeout_ms == 0 {
        cfg.opencode_zen.timeout_ms = DEFAULT_TIMEOUT_MS;
    }

    // logging defaults
    if cfg.logging.level.is_empty() {
        cfg.logging.level = DEFAULT_LOG_LEVEL.to_owned();
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate that required configuration fields are present.
fn validate(cfg: &Config, _path: &Path) -> Result<(), crate::error::CoreError> {
    if cfg.api_key.is_empty() {
        return Err(crate::error::CoreError::ConfigValidation {
            message: "api_key is required (set via config file or OC_GO_CC_API_KEY env var)"
                .to_owned(),
        });
    }
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use std::sync::Mutex;

    /// Serialize all tests that touch environment variables so they
    /// don't leak state into each other.  Tests that only call
    /// `Config::default()` are safe to run in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that saves an environment variable on creation and
    /// restores it (or removes it) on drop.
    struct EnvVarGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_owned(),
                original,
            }
        }

        fn remove(key: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self {
                key: key.to_owned(),
                original,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(val) => unsafe {
                    std::env::set_var(&self.key, val);
                },
                None => unsafe {
                    std::env::remove_var(&self.key);
                },
            }
        }
    }

    /// Holds the mutex lock and env-var guards together so that
    /// the lock is held for the entire lifetime of the env cleanup.
    struct EnvScope {
        _lock: std::sync::MutexGuard<'static, ()>,
        _guards: Vec<EnvVarGuard>,
    }

    /// Save all `OC_GO_CC_*` env overrides so the test runs in a
    /// clean environment.  The returned `EnvScope` holds a mutex lock
    /// that serialises all env-touching tests.
    fn save_oc_env() -> EnvScope {
        let lock = ENV_LOCK.lock().unwrap();
        let guards = vec![
            EnvVarGuard::remove("OC_GO_CC_API_KEY"),
            EnvVarGuard::remove("OC_GO_CC_HOST"),
            EnvVarGuard::remove("OC_GO_CC_PORT"),
            EnvVarGuard::remove("OC_GO_CC_OPENCODE_URL"),
            EnvVarGuard::remove("OC_GO_CC_OPENCODE_ZEN_URL"),
            EnvVarGuard::remove("OC_GO_CC_LOG_LEVEL"),
            EnvVarGuard::remove("OC_GO_CC_CONFIG"),
        ];
        EnvScope {
            _lock: lock,
            _guards: guards,
        }
    }

    // -- Legacy tests (must keep passing) -----------------------------------

    #[test]
    fn default_bind_is_localhost_or_all() {
        let cfg = Config::default();
        assert_eq!(cfg.bind.port(), 8080);
    }

    #[test]
    fn default_timeout_is_60_seconds() {
        let cfg = Config::default();
        assert_eq!(cfg.request_timeout, Duration::from_secs(60));
    }

    // -- New defaults tests -------------------------------------------------

    #[test]
    fn default_host_is_localhost() {
        let cfg = Config::default();
        assert_eq!(cfg.host, "127.0.0.1");
    }

    #[test]
    fn default_port_is_3456() {
        let cfg = Config::default();
        assert_eq!(cfg.port, 3456);
    }

    #[test]
    fn default_opencode_go_urls() {
        let cfg = Config::default();
        assert!(cfg.opencode_go.base_url.contains("opencode.ai"));
        assert!(cfg.opencode_go.anthropic_base_url.contains("opencode.ai"));
        assert_eq!(cfg.opencode_go.timeout_ms, 300_000);
    }

    #[test]
    fn default_opencode_zen_urls() {
        let cfg = Config::default();
        assert!(cfg.opencode_zen.base_url.contains("opencode.ai"));
        assert!(cfg.opencode_zen.responses_base_url.contains("opencode.ai"));
        assert!(cfg.opencode_zen.gemini_base_url.contains("opencode.ai"));
    }

    #[test]
    fn default_logging_is_info() {
        let cfg = Config::default();
        assert_eq!(cfg.logging.level, "info");
        assert!(!cfg.logging.requests);
    }

    #[test]
    fn default_models_and_fallbacks_empty() {
        let cfg = Config::default();
        assert!(cfg.models.is_empty());
        assert!(cfg.fallbacks.is_empty());
    }

    // -- JSON loading tests -------------------------------------------------

    #[test]
    fn load_valid_json_config() {
        let _env = save_oc_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"{{
  "api_key": "test-key-123",
  "host": "0.0.0.0",
  "port": 9999,
  "models": {{
    "default": {{
      "provider": "opencode-go",
      "model_id": "kimi-k2.6",
      "temperature": 0.7,
      "max_tokens": 4096
    }}
  }},
  "fallbacks": {{
    "default": [
      {{ "provider": "opencode-go", "model_id": "qwen3.6-plus" }}
    ]
  }},
  "logging": {{ "level": "debug", "requests": true }}
}}"#
        )
        .expect("write");

        let cfg = Config::load(&path).expect("load");
        assert_eq!(cfg.api_key, "test-key-123");
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 9999);
        assert_eq!(cfg.models["default"].model_id, "kimi-k2.6");
        assert_eq!(cfg.fallbacks["default"].len(), 1);
        assert_eq!(cfg.logging.level, "debug");
        assert!(cfg.logging.requests);
    }

    #[test]
    fn load_minimal_config_gets_defaults() {
        let _env = save_oc_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, r#"{{ "api_key": "key" }}"#).expect("write");

        let cfg = Config::load(&path).expect("load");
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 3456);
        assert_eq!(cfg.opencode_go.timeout_ms, 300_000);
        assert_eq!(cfg.logging.level, "info");
    }

    #[test]
    fn load_missing_api_key_fails() {
        let _env = save_oc_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, r#"{{ "host": "0.0.0.0" }}"#).expect("write");

        let result = Config::load(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("api_key is required"),
            "expected api_key validation error, got: {err}"
        );
    }

    #[test]
    fn load_nonexistent_file_fails() {
        let result = Config::load("/tmp/__llm_proxy_no_such_file__.json");
        assert!(result.is_err());
    }

    // -- Env interpolation tests --------------------------------------------

    #[test]
    fn env_var_interpolation_in_config() {
        let _env = save_oc_env();
        let _g = EnvVarGuard::set("LLM_PROXY_TEST_KEY", "interpolated-key");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, r#"{{ "api_key": "${{LLM_PROXY_TEST_KEY}}" }}"#).expect("write");

        let cfg = Config::load(&path).expect("load");
        assert_eq!(cfg.api_key, "interpolated-key");
    }

    #[test]
    fn unset_env_var_left_as_is() {
        let _env = save_oc_env();
        let _g = EnvVarGuard::remove("__LLM_PROXY_NEVER_SET__");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"{{ "api_key": "hardcoded", "host": "${{__LLM_PROXY_NEVER_SET__}}" }}"#
        )
        .expect("write");

        let cfg = Config::load(&path).expect("load");
        assert_eq!(cfg.host, "${__LLM_PROXY_NEVER_SET__}");
    }

    // -- Env override tests -------------------------------------------------

    #[test]
    fn env_overrides_take_precedence() {
        let _env = save_oc_env();
        let _g1 = EnvVarGuard::set("OC_GO_CC_API_KEY", "env-key");
        let _g2 = EnvVarGuard::set("OC_GO_CC_HOST", "10.0.0.1");
        let _g3 = EnvVarGuard::set("OC_GO_CC_PORT", "5555");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"{{ "api_key": "file-key", "host": "0.0.0.0", "port": 9999 }}"#
        )
        .expect("write");

        let cfg = Config::load(&path).expect("load");
        assert_eq!(cfg.api_key, "env-key");
        assert_eq!(cfg.host, "10.0.0.1");
        assert_eq!(cfg.port, 5555);
    }

    // -- ModelConfig / Thinking tests ---------------------------------------

    #[test]
    fn model_config_with_thinking() {
        let _env = save_oc_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"{{
  "api_key": "k",
  "models": {{
    "deepseek": {{
      "provider": "opencode-go",
      "model_id": "deepseek-v4-pro",
      "temperature": 0.7,
      "max_tokens": 8192,
      "reasoning_effort": "max",
      "thinking": {{ "type": "enabled" }}
    }}
  }}
}}"#
        )
        .expect("write");

        let cfg = Config::load(&path).expect("load");
        let m = &cfg.models["deepseek"];
        assert_eq!(m.reasoning_effort, "max");
        assert!(m.thinking.is_some());
        assert_eq!(m.thinking.as_ref().unwrap()["type"], "enabled");
    }

    #[test]
    fn model_config_without_thinking() {
        let _env = save_oc_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(
            f,
            r#"{{
  "api_key": "k",
  "models": {{
    "simple": {{
      "provider": "opencode-go",
      "model_id": "qwen3.5-plus",
      "temperature": 0.5,
      "max_tokens": 2048
    }}
  }}
}}"#
        )
        .expect("write");

        let cfg = Config::load(&path).expect("load");
        let m = &cfg.models["simple"];
        assert!(m.thinking.is_none());
        assert_eq!(m.context_threshold, 0);
    }

    // -- Serialization roundtrip --------------------------------------------

    #[test]
    fn config_roundtrip_json() {
        let mut cfg = Config {
            api_key: "roundtrip-key".to_owned(),
            ..Default::default()
        };
        cfg.models.insert(
            "default".to_owned(),
            ModelConfig {
                provider: "opencode-go".to_owned(),
                model_id: "glm-5.1".to_owned(),
                temperature: 0.7,
                max_tokens: 4096,
                context_threshold: 0,
                reasoning_effort: String::new(),
                thinking: None,
            },
        );

        let json = serde_json::to_string_pretty(&cfg).expect("serialize");
        // api_key is skip_serializing, so it must NOT appear in the output.
        assert!(
            !json.contains("roundtrip-key"),
            "api_key must not appear in serialized output"
        );
        let back: Config = serde_json::from_str(&json).expect("deserialize");
        // api_key is not serialized, so it will be empty on deserialization.
        assert_eq!(back.api_key, "");
        assert_eq!(back.models["default"].model_id, "glm-5.1");
    }

    /// Verify that Debug output does not leak the api_key.
    #[test]
    fn config_debug_redacts_api_key() {
        let cfg = Config {
            api_key: "super-secret-key-12345".to_owned(),
            ..Default::default()
        };
        let debug_output = format!("{:?}", cfg);
        assert!(
            !debug_output.contains("super-secret-key-12345"),
            "Debug output must not contain the actual api_key"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output should show [REDACTED] for api_key"
        );
    }
}

//! Shared environment-variable interpolation for configuration files.
//!
//! Provides a single [`interpolate_env_vars`] function and the compiled regex
//! used by both the old JSON config ([`crate::config`]) and the new TOML
//! provider config ([`crate::provider_config`]).

use regex::Regex;
use std::sync::OnceLock;

/// The compiled regex for `${VAR_NAME}` patterns.
///
/// Shared across all callers so the regex is compiled at most once per process.
static ENV_VAR_RE: OnceLock<Regex> = OnceLock::new();

/// Return the shared compiled regex for `${VAR_NAME}` patterns.
pub(crate) fn env_var_regex() -> &'static Regex {
    // SAFETY: this regex is a compile-time constant that is syntactically valid.
    // Failure here indicates a programming error in the regex literal, not a
    // runtime condition.
    ENV_VAR_RE.get_or_init(|| Regex::new(r"\$\{([A-Za-z0-9_]+)\}").expect("env var regex is valid"))
}

/// Replace `${ENV_VAR}` patterns in `input` with the value of the
/// corresponding environment variable.
///
/// Unset variables are left as-is (the `${VAR}` pattern remains in the output).
///
/// # Whole-file interpolation
///
/// This function operates on the entire raw file content before TOML/JSON
/// parsing. This means any `${VAR}` pattern anywhere in the file will be
/// interpolated, including in `api_key`, `endpoint`, `name`, and other string
/// fields. Single-braced patterns like `{model}` are **not** interpolated
/// (they use `{` not `${`).
pub fn interpolate_env_vars(input: &str) -> String {
    let re = env_var_regex();
    re.replace_all(input, |caps: &regex::Captures<'_>| {
        let var_name = &caps[1];
        std::env::var(var_name).unwrap_or_else(|_| caps[0].to_owned())
    })
    .into_owned()
}

/// Check whether `s` contains an unresolved `${VAR}` pattern.
///
/// Returns the first variable name found, or `None` if no pattern remains.
pub fn find_unresolved_env_var(s: &str) -> Option<String> {
    let re = env_var_regex();
    re.captures(s).map(|caps| caps[1].to_owned())
}

/// Find all `${VAR}` patterns in `s` and return them as `(var_name, resolved_value)` pairs.
///
/// This is used to track which env vars were interpolated so that empty
/// resolutions can be detected and reported distinctly from literal empty
/// strings.
pub fn find_env_var_refs(s: &str) -> Vec<(String, Option<String>)> {
    let re = env_var_regex();
    re.captures_iter(s)
        .map(|caps| {
            let var_name = caps[1].to_owned();
            let value = std::env::var(&var_name).ok();
            (var_name, value)
        })
        .collect()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_known_var() {
        let _g = crate::test_support::TestEnvLock::acquire();
        unsafe {
            std::env::set_var("_LLM_PROXY_TEST_INTERPOLATE", "hello");
        }
        let result = interpolate_env_vars("key = ${_LLM_PROXY_TEST_INTERPOLATE}");
        assert_eq!(result, "key = hello");
        unsafe {
            std::env::remove_var("_LLM_PROXY_TEST_INTERPOLATE");
        }
    }

    #[test]
    fn leaves_unknown_var_as_is() {
        let result = interpolate_env_vars("key = ${_LLM_PROXY_NEVER_EXISTS_12345}");
        assert_eq!(result, "key = ${_LLM_PROXY_NEVER_EXISTS_12345}");
    }

    #[test]
    fn find_unresolved_returns_var_name() {
        assert_eq!(
            find_unresolved_env_var("${MISSING_VAR}"),
            Some("MISSING_VAR".to_owned())
        );
    }

    #[test]
    fn find_unresolved_returns_none_when_resolved() {
        assert_eq!(find_unresolved_env_var("no vars here"), None);
    }

    // -- Non-matching patterns are left as-is ------------------------------------

    #[test]
    fn empty_braces_not_interpolated() {
        let result = interpolate_env_vars("key = ${}");
        assert_eq!(
            result, "key = ${}",
            "empty braces should not be interpolated"
        );
    }

    #[test]
    fn hyphenated_var_not_interpolated() {
        let result = interpolate_env_vars("key = ${my-var}");
        assert_eq!(
            result, "key = ${my-var}",
            "hyphenated var should not be interpolated"
        );
    }

    #[test]
    fn dotted_var_not_interpolated() {
        let result = interpolate_env_vars("key = ${my.var}");
        assert_eq!(
            result, "key = ${my.var}",
            "dotted var should not be interpolated"
        );
    }

    #[test]
    fn numeric_only_var_not_interpolated() {
        let result = interpolate_env_vars("key = ${123}");
        // ${123} does match [A-Za-z0-9_]+ since digits are allowed.
        // This is documented behavior: the regex allows numeric-only names.
        // If 123 is not set, it stays as-is.
        assert_eq!(result, "key = ${123}");
    }
}

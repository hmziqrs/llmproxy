//! Shared environment-variable interpolation for configuration files.
//!
//! Provides a single [`crate::env_interpolate::interpolate_env_vars`] function and the compiled regex
//! used by the TOML provider config ([`crate::provider_config`]).

use regex::Regex;

/// The compiled regex for `${VAR_NAME}` patterns.
///
/// Shared across all callers so the regex is compiled at most once per process.
/// Uses `LazyLock` for idiomatic one-time initialization.
static ENV_VAR_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // SAFETY: this regex is a compile-time constant that is syntactically valid.
    // Failure here indicates a programming error in the regex literal, not a
    // runtime condition.
    Regex::new(r"\$\{([A-Za-z0-9_]+)\}").expect("env var regex is valid")
});

/// Return the shared compiled regex for `${VAR_NAME}` patterns.
pub(crate) fn env_var_regex() -> &'static Regex {
    &ENV_VAR_RE
}

/// Replace `${ENV_VAR}` patterns in `input` with the value of the
/// corresponding environment variable.
///
/// Unset variables are left as-is (the `${VAR}` pattern remains in the output).
///
/// # Trust boundary
///
/// Values are substituted as raw text. Callers must ensure environment
/// variables contain no TOML-breaking characters if the output will be
/// parsed as TOML. A compromised or misconfigured environment variable
/// could inject arbitrary TOML structure. This is acceptable because the
/// process owner controls the environment.
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
        let _lock = crate::test_support::TestEnvLock::acquire();
        let _guard = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_INTERPOLATE", "hello");
        let result = interpolate_env_vars("key = ${_LLM_PROXY_TEST_INTERPOLATE}");
        assert_eq!(result, "key = hello");
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

    // -- Multiple interpolations in a single string -------------------------

    #[test]
    fn multiple_vars_in_single_string() {
        let _lock = crate::test_support::TestEnvLock::acquire();
        let _a = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_MULTI_A", "hello");
        let _b = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_MULTI_B", "world");
        let result =
            interpolate_env_vars("${_LLM_PROXY_TEST_MULTI_A} and ${_LLM_PROXY_TEST_MULTI_B}");
        assert_eq!(result, "hello and world");
    }

    #[test]
    fn mix_of_resolved_and_unresolved() {
        let _lock = crate::test_support::TestEnvLock::acquire();
        let _a = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_MIX_A", "yes");
        let result =
            interpolate_env_vars("${_LLM_PROXY_TEST_MIX_A} ${_LLM_PROXY_NEVER_EXISTS_MIX_B}");
        assert_eq!(result, "yes ${_LLM_PROXY_NEVER_EXISTS_MIX_B}");
    }

    // -- Env var set to empty string -----------------------------------------

    #[test]
    fn var_set_to_empty_string_replaces_with_empty() {
        let _lock = crate::test_support::TestEnvLock::acquire();
        let _guard = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_EMPTY_VAR", "");
        let result = interpolate_env_vars("key = ${_LLM_PROXY_TEST_EMPTY_VAR}");
        assert_eq!(result, "key = ");
    }

    // -- Adjacent and edge-case patterns -------------------------------------

    #[test]
    fn adjacent_patterns() {
        let _lock = crate::test_support::TestEnvLock::acquire();
        let _a = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_ADJ_A", "X");
        let _b = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_ADJ_B", "Y");
        let result = interpolate_env_vars("${_LLM_PROXY_TEST_ADJ_A}${_LLM_PROXY_TEST_ADJ_B}");
        assert_eq!(result, "XY");
    }

    #[test]
    fn dollar_sign_near_pattern() {
        let result = interpolate_env_vars("$$${_LLM_PROXY_NEVER_EXISTS_999}$$");
        // The inner ${...} is left as-is; surrounding $ characters are literal.
        // The regex matches ${_LLM_PROXY_NEVER_EXISTS_999} and replaces it with
        // itself (since it is unresolved), leaving the surrounding $$ intact.
        assert_eq!(result, "$$${_LLM_PROXY_NEVER_EXISTS_999}$$");
    }

    // -- find_env_var_refs tests ---------------------------------------------

    #[test]
    fn find_env_var_refs_multiple_matches() {
        let _lock = crate::test_support::TestEnvLock::acquire();
        let _guard_a = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_REFS_A", "val_a");
        let refs = find_env_var_refs("${_LLM_PROXY_TEST_REFS_A} and ${_LLM_PROXY_TEST_REFS_B}");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, "_LLM_PROXY_TEST_REFS_A");
        assert_eq!(refs[0].1, Some("val_a".to_owned()));
        assert_eq!(refs[1].0, "_LLM_PROXY_TEST_REFS_B");
        // Not set, so resolved value is None.
        assert_eq!(refs[1].1, None);
    }

    #[test]
    fn find_env_var_refs_empty_var() {
        let _lock = crate::test_support::TestEnvLock::acquire();
        let _guard = crate::test_support::EnvVarGuard::set("_LLM_PROXY_TEST_REFS_EMPTY", "");
        let refs = find_env_var_refs("${_LLM_PROXY_TEST_REFS_EMPTY}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, "_LLM_PROXY_TEST_REFS_EMPTY");
        // Set but empty: Some("").
        assert_eq!(refs[0].1, Some(String::new()));
    }

    #[test]
    fn find_env_var_refs_no_matches() {
        let refs = find_env_var_refs("no patterns here");
        assert!(refs.is_empty());
    }

    #[test]
    fn find_env_var_refs_duplicate_names() {
        let refs = find_env_var_refs("${DUP} and ${DUP}");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, "DUP");
        assert_eq!(refs[1].0, "DUP");
    }
}

//! Upstream provider model discovery.

use std::time::Duration;

use futures::TryStreamExt;
use llm_proxy_core::{
    AuthStyle, ProviderConfig, ProviderDiscoveryKind, ProviderRouteKind, StaticModelCatalogEntry,
};
use reqwest::{Client, Url};
use serde_json::Value;

use crate::ProviderError;

const MAX_PAGES: usize = 100;
const MAX_MODELS: usize = 20_000;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// GET-capable HTTP client for provider model discovery.
#[derive(Debug, Clone)]
pub struct DiscoveryClient {
    http: Client,
}

impl DiscoveryClient {
    /// Build a discovery client with short, independent timeouts.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn try_new() -> Result<Self, ProviderError> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("too many discovery redirects");
                }
                match attempt.url().scheme() {
                    "http" | "https" => attempt.follow(),
                    _ => attempt.stop(),
                }
            }))
            .build()?;
        Ok(Self { http })
    }

    /// Fetch and normalize the configured provider model list.
    ///
    /// # Errors
    ///
    /// Returns an error for missing discovery config, invalid responses,
    /// pagination failures, or transport errors.
    pub async fn discover(
        &self,
        provider: &ProviderConfig,
    ) -> Result<Vec<StaticModelCatalogEntry>, ProviderError> {
        let discovery = provider.discovery.as_ref().ok_or_else(|| {
            ProviderError::InvalidConfig(format!(
                "provider \"{}\" has no discovery configuration",
                provider.name
            ))
        })?;

        let mut url = Url::parse(&discovery.endpoint).map_err(|error| {
            ProviderError::InvalidConfig(format!("invalid discovery endpoint: {error}"))
        })?;
        let mut models = Vec::new();

        for _ in 0..MAX_PAGES {
            let value = self.fetch_page(provider, &url).await?;
            parse_models(discovery.kind, &value, &mut models)?;
            if models.len() > MAX_MODELS {
                return Err(ProviderError::InvalidConfig(format!(
                    "provider \"{}\" discovery exceeded {MAX_MODELS} models",
                    provider.name
                )));
            }

            let Some((name, token)) = next_page(discovery.kind, &value) else {
                models.sort_by(|a, b| a.id.cmp(&b.id));
                models.dedup_by(|a, b| a.id == b.id);
                return Ok(models);
            };
            set_query_parameter(&mut url, name, &token);
        }

        Err(ProviderError::InvalidConfig(format!(
            "provider \"{}\" discovery exceeded {MAX_PAGES} pages",
            provider.name
        )))
    }

    async fn fetch_page(
        &self,
        provider: &ProviderConfig,
        url: &Url,
    ) -> Result<Value, ProviderError> {
        let discovery = provider.discovery.as_ref().ok_or_else(|| {
            ProviderError::InvalidConfig("missing discovery configuration".to_owned())
        })?;
        let mut request = self.http.get(url.clone());
        request = match provider.auth_style {
            AuthStyle::Bearer => request.bearer_auth(&provider.api_key),
            AuthStyle::XApiKey => request.header("x-api-key", &provider.api_key),
            AuthStyle::XGoogleApiKey => request.header("x-goog-api-key", &provider.api_key),
            AuthStyle::Both => request
                .bearer_auth(&provider.api_key)
                .header("x-api-key", &provider.api_key),
        };
        for (name, value) in &discovery.headers {
            request = request.header(name, value);
        }

        let response = request.send().await?;
        let status = response.status();
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.try_next().await? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(ProviderError::InvalidConfig(
                    "discovery response exceeded 4 MiB".to_owned(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(ProviderError::api(
                status.as_u16(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
        serde_json::from_slice(&bytes).map_err(ProviderError::from)
    }
}

fn parse_models(
    kind: ProviderDiscoveryKind,
    value: &Value,
    output: &mut Vec<StaticModelCatalogEntry>,
) -> Result<(), ProviderError> {
    let records = match kind {
        ProviderDiscoveryKind::GeminiModels => value.get("models"),
        ProviderDiscoveryKind::FireworksAccountModels => {
            value.get("models").or_else(|| value.get("data"))
        }
        _ => value.get("data"),
    }
    .and_then(Value::as_array);

    let Some(records) = records else {
        return Err(ProviderError::InvalidConfig(
            "discovery response has an invalid top-level model list".to_owned(),
        ));
    };
    for record in records {
        let id = record
            .get("id")
            .or_else(|| record.get("name"))
            .and_then(Value::as_str);
        let Some(id) = id.filter(|id| !id.trim().is_empty()) else {
            tracing::warn!("ignoring malformed discovery model record without an id");
            continue;
        };
        if kind == ProviderDiscoveryKind::GeminiModels {
            let supports_generate = record
                .get("supportedGenerationMethods")
                .or_else(|| record.get("supportedActions"))
                .and_then(Value::as_array)
                .is_some_and(|actions| actions.iter().any(|action| action == "generateContent"));
            if !supports_generate {
                tracing::debug!(
                    model = id,
                    "ignoring Gemini model without generateContent support"
                );
                continue;
            }
        }
        output.push(StaticModelCatalogEntry {
            id: id.to_owned(),
            display_name: record
                .get("display_name")
                .or_else(|| record.get("displayName"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            supports: vec![
                ProviderRouteKind::ChatCompletions,
                ProviderRouteKind::Messages,
            ],
            context_length: record
                .get("context_length")
                .or_else(|| record.get("inputTokenLimit"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
        });
    }
    Ok(())
}

fn set_query_parameter(url: &mut Url, name: &str, value: &str) {
    let existing: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != name)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    let mut query = url.query_pairs_mut();
    for (key, value) in existing {
        query.append_pair(&key, &value);
    }
    query.append_pair(name, value);
}

fn next_page(kind: ProviderDiscoveryKind, value: &Value) -> Option<(&'static str, String)> {
    match kind {
        ProviderDiscoveryKind::GeminiModels => value
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(|token| ("pageToken", token.to_owned())),
        ProviderDiscoveryKind::AnthropicModels => value
            .get("has_more")
            .and_then(Value::as_bool)
            .filter(|has_more| *has_more)
            .and_then(|_| value.get("last_id").and_then(Value::as_str))
            .map(|token| ("after_id", token.to_owned())),
        ProviderDiscoveryKind::FireworksAccountModels => value
            .get("nextPageToken")
            .or_else(|| value.get("next_page_token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(|token| ("pageToken", token.to_owned())),
        _ => None,
    }
}

impl Default for DiscoveryClient {
    fn default() -> Self {
        Self::try_new().expect("default discovery HTTP client configuration is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_records_are_sorted_and_malformed_records_are_skipped() {
        let mut models = Vec::new();
        parse_models(
            ProviderDiscoveryKind::OpenAiCompatibleModels,
            &json!({"data": [{"id": "z"}, {"missing": "id"}, {"id": "a"}]}),
            &mut models,
        )
        .expect("valid response");
        models.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
    }

    #[test]
    fn invalid_top_level_shape_fails() {
        let error = parse_models(
            ProviderDiscoveryKind::OpenAiModels,
            &json!({"models": []}),
            &mut Vec::new(),
        )
        .expect_err("invalid shape");
        assert!(error.to_string().contains("top-level"));
    }

    #[test]
    fn gemini_filters_non_generation_models() {
        let mut models = Vec::new();
        parse_models(
            ProviderDiscoveryKind::GeminiModels,
            &json!({"models": [
                {"name": "models/embed", "supportedGenerationMethods": ["embedContent"]},
                {"name": "models/generate", "supportedGenerationMethods": ["generateContent"]}
            ]}),
            &mut models,
        )
        .expect("valid response");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "models/generate");
    }

    #[test]
    fn pagination_parameter_is_replaced() {
        let mut url = Url::parse("https://example.com/models?pageToken=old&limit=10").unwrap();
        set_query_parameter(&mut url, "pageToken", "new");
        assert_eq!(
            url.as_str(),
            "https://example.com/models?limit=10&pageToken=new"
        );
    }
}

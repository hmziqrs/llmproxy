# Mini Quirks — Search Bridges

## Citations and search results are response side channels

Perplexity-style responses attach citations/search results outside normal
assistant text, and then derive OpenAI annotations by matching `[1]` markers
inside the message content.

Design rule:

```text
Citations are structured metadata, not just text post-processing.
```

Core response should preserve:

```text
raw_citations
search_results
text_annotations
annotation_source = provider | inferred_from_text
```

When translating to a protocol without annotations, emit a warning instead
of flattening citations invisibly.

## Parallel AI search remaps unified query/filter fields into `objective` and `source_policy`

The Parallel AI search adapter does not forward the generic search request
shape unchanged. A list of query strings is collapsed into a single
`objective`, while the unified domain filters are rewritten into
`source_policy.allowed_domains` and `source_policy.disallowed_domains`.
Everything else is passed through only after that provider-specific mapping,
so the adapter is doing a real search-protocol translation rather than a thin
HTTP proxy.

Design rule:

```text
Search adapters often need to translate both query intent and source policy
before the upstream API can understand the request.
```

Track:

```text
parallel_ai_search_objective_from_query_list
parallel_ai_search_source_policy_domain_mapping
parallel_ai_search_query_pass_through_after_remap
```

## Parallel AI search requires a beta feature header

Parallel AI search is gated behind a specific beta header. The adapter always
adds `parallel-beta: search-extract-2025-10-10` when building the request,
which means the feature contract is not just the endpoint path and body
shape; it also depends on a transport header.

Design rule:

```text
When an API feature is behind a beta header, that header belongs in the
normalized contract alongside the path and body mapping.
```

Track:

```text
parallel_ai_search_beta_header
parallel_ai_search_transport_gating
```

## Perplexity chat turns citations and search counts into billing metadata

Perplexity chat computes its own usage side channels. It estimates
`citation_tokens` from citation text, pulls search-query counts from either
`usage.num_search_queries`, root `num_search_queries`, `usage.search_queries`,
or root `search_queries`, and stores the result in
`usage.prompt_tokens_details.web_search_requests` so the cost calculator can
charge for it. The same adapter also converts citations and search results
into OpenAI annotations.

Design rule:

```text
Search-powered responses often need synthetic usage fields so downstream cost
logic can see what the provider actually billed.
```

Track:

```text
perplexity_citation_token_estimate
perplexity_web_search_request_count
perplexity_usage_prompt_tokens_details
perplexity_url_citation_annotations
```

## DuckDuckGo search is a GET/query-string bridge with list-query collapsing and nested result flattening

DuckDuckGo does not speak the same request shape as the rest of the search
providers. The adapter forces GET, turns a list of search queries into a
single joined string, builds the request as query parameters on the URL,
always sends `format=json`, and keeps `max_results` as a private filter rather
than a native API field. The response path then flattens `AbstractURL`,
`AbstractText`, and nested `RelatedTopics.Topics` into a single LiteLLM
`SearchResponse`, splitting `Text` into title/snippet when needed.

Design rule:

```text
Search bridges can be query-string protocols, not JSON-body protocols, and
their result trees may need to be flattened before they resemble the shared
search schema.
```

Track:

```text
duckduckgo_get_query_string_request
duckduckgo_list_query_collapse
duckduckgo_private_max_results_filter
duckduckgo_nested_related_topics_flattening
duckduckgo_abstract_result_preference
```

## Brave Search is a query-string protocol with built-in domain rewrites and metadata defaults

Brave Search is another GET-based search bridge, but its quirks are different
from DuckDuckGo’s. The adapter turns list queries into one string, sets
`include_fetch_metadata=True` by default unless explicitly disabled, caps
`max_results` at 20, rewrites domain filters into `site:` clauses inside the
query itself, and sends the final request as query parameters on the URL.
On the response side, it flattens `AbstractURL` and `RelatedTopics` trees into
LiteLLM search results and parses Brave’s timestamps into ISO dates when
possible.

Design rule:

```text
Search adapters can encode filtering policy directly into the query string
and still need response-time normalization for timestamps and nested result
trees.
```

Track:

```text
brave_get_query_string_request
brave_include_fetch_metadata_default
brave_max_results_cap
brave_domain_filter_site_clause_rewrite
brave_nested_topics_flattening
brave_timestamp_to_iso_date
```

## Exa AI search rewrites unified filters and forces text content on by default

The Exa AI search adapter collapses list
queries into one string, remaps `max_results` to `numResults`, turns unified
domain and country filters into `includeDomains` and `userLocation`, and
injects `contents={"text": true}` unless the caller already asked for a
different content shape. That last step matters because Exa does not return
text by default. The response path then maps `results[].text` and
`publishedDate` into the shared search schema.

Design rule:

```text
Search adapters sometimes have to request extra content up front or the
response will be structurally incomplete for the downstream schema.
```

Track:

```text
exa_query_list_collapse
exa_max_results_to_num_results
exa_domain_filter_include_domains
exa_country_to_user_location
exa_contents_text_default
exa_published_date_to_result_date
```

## Serper search rewrites the query string and folds domain filters into `site:` clauses

Serper is another search bridge with its own request contract. The adapter
collapses list queries into a single string, maps `max_results` to `num`,
lowercases `country` into `gl`, and folds `search_domain_filter` into the
query itself as `site:` clauses joined with `OR`. The response path then maps
`organic[].title`, `organic[].link`, `organic[].snippet`, and optional
`organic[].date` into the shared search schema.

Design rule:

```text
Search adapters can encode domain restrictions into the query string itself
when the upstream API does not support a first-class domain filter field.
```

Track:

```text
serper_query_list_collapse
serper_max_results_to_num
serper_country_to_gl
serper_domain_filter_site_clauses
serper_organic_result_mapping
```

## DataForSEO is a POST-based SERP bridge with task arrays, location defaults, and organic-only result flattening

DataForSEO does not accept the generic search schema as-is. The adapter uses
HTTP Basic Auth built from `login:password`, posts a list of task objects
instead of a single JSON object, collapses list queries to a single keyword,
maps `max_results` to `depth` with a hard cap of 700, fills in a default
language code and location code when the caller omits them, and sends the
search filters in DataForSEO’s own fields. The response path then unwraps
`tasks[0].result[0].items[]` and keeps only organic results when building the
shared `SearchResponse`.

Design rule:

```text
Search adapters can require batched task envelopes and provider-default
location/language values before the upstream search API will respond usefully.
```

Track:

```text
dataforseo_basic_auth_login_password
dataforseo_task_array_request
dataforseo_keyword_from_query
dataforseo_depth_cap_700
dataforseo_location_language_defaults
dataforseo_organic_only_result_flattening
```

## Anthropic feature flags synthesize beta headers from files, tool search, code execution, and skills usage

Anthropic does not treat beta headers as a static string. The shared header
builder inspects the active request shape and emits different beta flags based
on the features in play: files add `files-api-2025-04-14` plus
`code-execution-2025-05-22`, MCP adds `mcp-client-2025-04-04`, tool search /
programmatic tool calling / `input_examples` all share the Anthropic tool-search
beta, code execution tools add `code-execution-2025-08-25`, and any container
with skills adds `skills-2025-10-02`. On Vertex requests the builder stops
emitting the normal Anthropic beta set and only preserves the web-search beta
when required.

Design rule:

```text
Header synthesis is feature detection, not a fixed config string.
```

Track:

```text
anthropic_beta_from_file_id
anthropic_beta_from_tool_search
anthropic_beta_from_programmatic_tool_calling
anthropic_beta_from_input_examples
anthropic_beta_from_code_execution
anthropic_beta_from_container_skills
anthropic_vertex_beta_filtering
```

## Anthropic web-search-only requests short-circuit into a synthetic response before backend dispatch

The Anthropic messages handler does not always send web-search requests to the
model backend. It first runs a web-search interception hook, and when the
request matches the web-search-only pattern the handler executes the search
directly through Tavily or Perplexity and returns a synthetic Anthropic
response without touching the LLM path at all. If the caller asked for
streaming, the synthetic response is wrapped in the fake Anthropic stream
iterator so the caller still sees the expected stream contract.

Design rule:

```text
Specialized assistant flows can be intercepted and satisfied entirely outside the backend model.
```

Track:

```text
anthropic_websearch_short_circuit
anthropic_websearch_synthetic_response
anthropic_websearch_stream_wrapper
anthropic_websearch_backend_bypass
```

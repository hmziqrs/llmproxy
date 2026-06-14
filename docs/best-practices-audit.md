# Best-Practices Audit — llm-proxy

**Scope:** `/Users/hmziq/os/llm-proxy` (Rust edition 2024, ~39K lines, 7 crates). Findings re-derived from a fresh read of the source, not the prior `docs/audit-report.md` (643 findings) — items already addressed there were excluded.

**Headline numbers:** 42 verified findings across two passes (2 HIGH; 7 MEDIUM; 31 LOW; 2 INFO). The two HIGH production-availability defects from the first pass still dominate: an **unbounded graceful-shutdown drain** that a single stuck SSE stream can pin indefinitely, and a **zero-duration `request_timeout`** that silently 408s every API route with no validation guard. The second pass widened coverage from 6 to 16 dimensions and surfaced four new MEDIUM items: per-request **early `serde_json::Map` allocation** on the chat-decode hot path, **client-disconnect events corrupting the failure-rate SLO**, a **testability gap** that leaves `cmd_stop`/`cmd_status` command logic untested, and **`TimeoutLayer`/`DefaultBodyLimit` emitting plain-text bodies that break the protocol-shaped JSON error schema**. The remaining additions are low-impact idiomatic-Rust, test-quality, and broken-intra-doc-link findings. A **third pass** (see [Coverage Gaps (Third Pass)](#coverage-gaps-third-pass) below) then mapped what the first two had *structurally missed* — two entire Apollo skill chapters (Ch4 error-handling, Ch6 dispatch), the secret-redaction surface, untrusted-upstream robustness, and the CLI/binary crate — surfacing **4 more MEDIUM** (a client-facing secret-redaction gap, an SSE-parser OOM, a daemon-log TOCTOU, and an `autostart` clobber) and **15 more LOW**.

## Severity Counts

| Severity | Count | Notes |
|---|---|---|
| HIGH | 2 | Unbounded shutdown drain; zero-timeout DoS path |
| MEDIUM | 7 | request-id/span correlation; slowloris surface; clippy-gate break; early-allocation on decode hot path; client-disconnect metric conflation; cmd_stop/cmd_status testability gap; TimeoutLayer/DefaultBodyLimit non-JSON error bodies |
| LOW | 31 | Performance nits, lint hygiene, security headers, panic guard, health telemetry, idiomatic-Rust (borrowing/owned params, redundant clones), test quality (snapshot/blob/multi-behavior/flaky-timing/missing-doc-test), broken intra-doc links, method-routing, handler ordering, log-level inconsistency |
| INFO | 2 | Misleading 'Arc-like' comment; `Arc<Counter>` over a zero-sized type |
| **Total verified** | **42** | |

(7 uncertain and 41 refuted candidates were excluded across both passes — see Verification Notes.)

---

## Findings by Severity

### HIGH-1 — Graceful shutdown has no hard deadline; long SSE streams can block shutdown forever

- **File:** `apps/llm-proxy/src/commands/serve.rs:97-105`
- **Dimension:** Axum / production-shutdown
- **Evidence**
  ```rust
  let result = axum::serve(
      listener,
      app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
  )
  .with_graceful_shutdown(shutdown_signal())
  .await;
  ```
- **Why it matters:** axum 0.8's `with_graceful_shutdown` waits indefinitely for in-flight connections to drain. The proxy serves long-lived streaming responses (`core_pipeline.rs:567` `handle_core_stream` returns a `Response<Body>` with a 3s heartbeat that the upstream can hold open arbitrarily), and `routes/mod.rs:64-66` itself documents that the per-request `TimeoutLayer` does **not** bound the streaming response body. On SIGTERM/SIGINT from an orchestrator (Kubernetes/LB), one stuck upstream stream pins the whole process. There is no `tokio::time::timeout`, no `shutdown_timeout`, no `drain()`-with-deadline anywhere in the server crate or binary.
- **Recommendation:** Wrap the serve future in `tokio::time::timeout(Duration::from_secs(30), axum::serve(...).with_graceful_shutdown(shutdown_signal()))`; log and force-exit (`std::process::exit`) if the deadline elapses. Make the deadline configurable.
- **Verifier note:** Confirmed against source; no `cfg(test)`, no config guard, no documented exception. `shutdown_signal()` returns on signal without imposing a deadline.

---

### HIGH-2 — Zero-duration `request_timeout` returns 408 for every API request, unguarded

- **File:** `crates/llm-proxy-server/src/routes/mod.rs:88-91` (build site); `crates/llm-proxy-core/src/provider_config.rs:43-44` (deserialize site); `crates/llm-proxy-server/src/state.rs:154-156, 582-593` (test proving zero round-trips).
- **Dimension:** Axum / middleware-layers
- **Evidence**
  ```rust
  let api_middleware = ServiceBuilder::new()
      .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
      .layer(TraceLayer::new_for_http())
      .layer(TimeoutLayer::with_status_code(
          StatusCode::REQUEST_TIMEOUT,
          timeout,
      ));
  ```
- **Why it matters:** `request_timeout` is deserialized via `humantime_serde` with no bounds. `load_app_config` (`provider_config.rs:949-993`) validates `hot_reload` but never inspects `request_timeout`; the `validate` command only prints the value. tower-http 0.6's `Timeout` builds `tokio::time::sleep(self.timeout)` on every call and, if ready before the inner handler is polled, returns the configured status (`tower-http-0.6.11/src/timeout/service.rs:112-119,140-149`). `tokio::time::sleep(Duration::ZERO)` is immediately ready, so `request_timeout = "0s"` in TOML silently bricks **every** API route with 408 before any handler runs. The test `zero_duration_timeout_is_returned` asserts this end-to-end.
- **Recommendation:** Clamp in `build_router` or `ServerConfig` validation: `let timeout = if timeout.is_zero() { Duration::from_secs(300) } else { timeout };` — or reject zero/sub-second values at config load with a clear error. Log the resolved timeout at startup. (See also the companion LOW finding at `state.rs:154-156`.)
- **Verifier note:** Verified by reading tower-http source; the finding's line cite (113-145) is slightly off (load-bearing lines 112-119, 140-149) but substance is exact.

---

### MEDIUM-1 — Generated `x-request-id` is not propagated into the `TraceLayer` span

- **File:** `crates/llm-proxy-server/src/routes/mod.rs:75, 87`
- **Dimension:** Axum / middleware-layers (trace-layer-observability)
- **Evidence:** both routers use bare `.layer(TraceLayer::new_for_http())` with no `make_span_with`.
- **Why it matters:** The proxy generates a request id inside the handler (`core_pipeline.rs:226` `RequestIdGenerator::next_id`) and attaches it to responses as `x-request-id` (`core_pipeline.rs:511-516`, `:727-731`). But the id is created *after* `TraceLayer` opens its span, so spans carry only method/uri/version (`DefaultMakeSpan` defaults). Handler logs emit `request_id` as a standalone event field (`core_pipeline.rs:500, :733`) but not nested in a span carrying the id — breaking distributed-trace/span correlation. Grep confirms no `make_span_with`, no `DefaultMakeSpan` override, no outermost `from_fn` pre-injecting the id into extensions.
- **Recommendation:** Add a `middleware::from_fn` **before** (outermost of) `TraceLayer` that generates the id and inserts it into request extensions, then configure `TraceLayer::new_for_http().make_span_with(|req: &Request<_>| info_span!("request", request_id = %req.extensions().get::<String>().cloned().unwrap_or_default()))`. Idiomatic Axum pattern for end-to-end correlation.
- **Verifier note:** Production router code (not `cfg(test)`); partial correlation by grep is still possible via the event field, hence MEDIUM rather than HIGH.

---

### MEDIUM-2 — No TCP keepalive / read-header timeout on the listener (slowloris surface)

- **File:** `apps/llm-proxy/src/commands/serve.rs:88-103`
- **Dimension:** Axum / production-shutdown
- **Evidence:** bare `TcpListener::bind(...)` passed straight to `axum::serve`; no `socket2`, no `set_tcp_keepalive`, no `TCP_NODELAY`, no `Accept` wrapper, no connection cap (workspace-wide grep: zero hits).
- **Why it matters:** `TimeoutLayer` only covers the fully-buffered handler body on API routes, not the header-read phase. A drip-fed-header client can pin a connection outside `TimeoutLayer`'s reach; with no cap, many such connections exhaust resources. Hyper imposes no default read-header timeout.
- **Recommendation:** Configure the listener via `socket2` for TCP keepalive / `TCP_NODELAY`, or impose a read-header timeout via a connection wrapper. At minimum document that a reverse proxy (nginx/envoy) is required for connection-level slowloris protection in production.
- **Verifier note:** Kept at MEDIUM (not escalated) because exposure is narrow for a typically-loopback developer-facing proxy, but the hardening gap is genuine and unmitigated.

---

### MEDIUM-3 — 9 deprecated `is_duplicate` calls in tests break `cargo clippy -D warnings`

- **File:** `crates/llm-proxy-server/src/middleware.rs:82` (deprecation); `:403, 409, 410, 416, 417, 477, 478, 484, 485` (call sites)
- **Dimension:** Apollo / clippy-linting
- **Evidence:**
  ```rust
  #[deprecated(note = "use is_duplicate_with_path instead")]  // line 82
  pub fn is_duplicate(&self, body: &[u8]) -> bool { self.is_duplicate_with_path("", body) }
  // ...
  fn dedup_new_request_is_not_duplicate() {
      let dedup = RequestDeduplicator::new();
      assert!(!dedup.is_duplicate(b"hello")); // +8 more through line 485
  }
  ```
- **Why it matters:** `cargo clippy -p llm-proxy-server --tests -- -D warnings` (and the workspace-wide variant) fails to compile the lib-test target with exactly 9 `use of deprecated method ... is_duplicate` errors. The replacement `is_duplicate_with_path` is already fully covered by dedicated tests at `middleware.rs:460-471`, so the 9 assertions are redundant as well as gate-breaking. If `-D warnings` were enforced in CI, the gate would fail.
- **Recommendation:** Either (a) migrate the assertions to `is_duplicate_with_path("", b"hello")` and delete the redundant deprecated tests, or (b) if the deprecated API must stay covered, add `#[expect(deprecated)]` on the specific test fns with a comment.
- **Verifier note:** Reproduced both clippy invocations returning exactly 9 errors; no `#[allow]`/`#[expect]` mitigation in the file. Confined to `#[cfg(test)]`, hence MEDIUM rather than HIGH.

---

### MEDIUM-4 — Request-decode path eagerly allocates an empty `serde_json::Map` via `unwrap_or` on the `Some` branch

- **File:** `crates/llm-proxy-protocol/src/client/openai_chat.rs:167` (also `:236`; `crates/llm-proxy-protocol/src/client/anthropic.rs:226`)
- **Dimension:** Apollo / idioms-ownership (early-allocation)
- **Evidence:**
  ```rust
  .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));   // openai_chat.rs:167
  ```
- **Why it matters:** `Option::unwrap_or` takes `T` by value (not `FnOnce`), so `serde_json::Map::new()` is eagerly constructed on **every** call and thrown away on the `Some` arm. `serde_json::Map` is heap-backed (`BTreeMap` / `IndexMap`), so this is a needless heap allocation on the happy path — the canonical case chapter_01 §1.4 calls out for `unwrap_or_else`. This sits on the inbound request-decode hot path: `openai_chat::decode_request` is called from `routes/chat.rs:74` (`/v1/chat/completions`), `anthropic::decode_request` from `routes/messages.rs:74` (`/v1/messages`) — both once per request. In the common tool-calling case the arguments/parameters/input are present and valid, so the allocated empty `Map` is discarded on most decodes. The identical pattern recurs at `openai_chat.rs:236` (tool `parameters`) and `anthropic.rs:226` (tool_use `input`); a workspace grep confirms these are the only three occurrences.
- **Recommendation:** Replace all three with `.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))`, or simply `.unwrap_or_default()` since `serde_json::Value: Default`.
- **Verifier note:** All three sites present exactly as claimed; verified directly. `#[cfg(test)]` does not guard the production functions (only one test caller exists at `openai_chat.rs:958`). Raw runtime impact is modest (one small empty-Map allocation discarded per affected decode), so MEDIUM is the correct floor.

---

### MEDIUM-5 — Client-initiated disconnects are recorded as request failures, corrupting error-rate metrics

- **File:** `crates/llm-proxy-server/src/routes/core_pipeline.rs:797, 804, 859`
- **Dimension:** Axum / responses-streaming (streaming-metrics-correctness)
- **Evidence:**
  ```rust
  // emit_event, first-byte path (line 797):
  // Receiver dropped -- handler is gone (client disconnect or timeout)...
  self.stream_metrics.metrics.record_failure();
  // emit_event, post-first-byte path (line 800-804):
  } else if tx.send(event).await.is_err() {
      // Channel receiver dropped -- the SSE handler task has exited
      // (client disconnect or timeout)...
      self.stream_metrics.metrics.record_failure();
  // select! cancel arm (line 856-865):
  _ = cancel_clone.cancelled() => {
      // Client disconnected; abort upstream stream.
      ctx.stream_metrics.metrics.record_failure();
  ```
- **Why it matters:** Three disconnect paths all call `record_failure()`, which unconditionally increments both `requests_failed` and `upstream_calls` (`metrics.rs:104-107`). The `Metrics` struct (`metrics.rs:37-49`) has **no cancellation counter** — there is no `record_client_cancel()` / `cancelled` field. So a user pressing "stop" in their UI, or a timeout-layer teardown, increments the same counters as a genuine upstream 5xx or decode error, corrupting the error-rate SLO and any alerting keyed off it.
- **Recommendation:** Introduce a `Metrics::record_client_cancel()` counter (or a `cancelled` field) and call it from all three disconnect paths (the cancel arm and the two `tx.send`/`fb_tx.send` `is_err` arms), reserving `record_failure()` for genuine upstream/decode errors.
- **Verifier note:** Verified at all three sites; `Metrics` confirms no cancellation counter exists. Not a duplicate of audit-report #73 (that finding is about missing provider/model attribution in `record_failure`, a different concern). No `#[cfg(test)]` guard. MEDIUM: operational-telemetry correctness defect, but no request-correctness, data-integrity, or security impact.

---

### MEDIUM-6 — `cmd_stop` and `cmd_status` are untestable because they hardcode the global `config_dir`; integration tests explicitly punt to the underlying predicate

- **File:** `apps/llm-proxy/src/commands/stop.rs:10` (`cmd_stop`); `apps/llm-proxy/src/commands/status.rs:16` (`cmd_status`); `apps/llm-proxy/src/pid.rs:22-31` (`pid_manager()` → `config_dir()`); test admission at `apps/llm-proxy/tests/cli_stop.rs:11` and `apps/llm-proxy/tests/cli_status.rs:4`
- **Dimension:** Apollo / testing (testability-gap)
- **Evidence:**
  ```rust
  // apps/llm-proxy/tests/cli_stop.rs:11-14
  // We cannot call cmd_stop() directly because it reads the PID file from the global config_dir.
  // Instead verify the underlying check: no file -> read_pid returns None -> stop returns Ok.
      let dir = tempfile::tempdir().unwrap();
      let mgr = llm_proxy_core::PidManager::new(dir.path());
      let pid = mgr.read_pid().unwrap();
      assert_eq!(pid, None, ...);
  ```
- **Why it matters:** `cmd_stop()` / `cmd_status()` build their `PidManager` via `pid_manager()` → `config_dir()`, which reads `$HOME`/`$USERPROFILE` from the process environment and accepts no base-dir parameter. The injectable layer already exists — `llm_proxy_core::PidManager::new(config_dir: impl Into<PathBuf>)` (`crates/llm-proxy-core/src/pid.rs:25`) — but the command wrappers discarded it. As a result the command-level branching is genuinely untested: the stale-file cleanup path (`stop.rs:18-22`), the "no PID file found" message (`stop.rs:71-73`), and the full SIGTERM→poll-loop→SIGKILL escalation (`stop.rs:24-69`) are never exercised. This is exactly the chapter_05 §5.3 guidance (keep side-effecting logic minimal and injectable) being violated.
- **Recommendation:** Refactor `cmd_stop`/`cmd_status` to accept a `&PidManager` (or a `paths`/`ConfigDirs` struct) so the full command is exercised against a tempdir-backed manager.
- **Verifier note:** Every claimed point confirmed by reading the code; the test comments are verbatim. No correctness defect, but material branching logic in two CLI commands is uncovered by tests. MEDIUM is appropriate.

---

### MEDIUM-7 — `TimeoutLayer` and `DefaultBodyLimit` emit non-JSON error bodies, breaking the protocol-shaped JSON error schema

- **File:** `crates/llm-proxy-server/src/routes/mod.rs:88-91` (layer stack); tower-http behavior at `tower-http-0.6.11/src/timeout/service.rs:140-150`; axum `Bytes`/`LengthLimitError` rejection at `axum-core-0.5.5/src/macros.rs:62-73`
- **Dimension:** Axum / error-handling-intoresponse (inconsistent-error-body-schema)
- **Evidence:**
  ```rust
  let api_middleware = ServiceBuilder::new()
      .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
      .layer(TraceLayer::new_for_http())
      .layer(TimeoutLayer::with_status_code(
          StatusCode::REQUEST_TIMEOUT,
          timeout,
      ));
  ```
- **Why it matters:** Every handler-produced error goes through `route_error_response` (`routes/messages.rs:38-42`, `routes/chat.rs:40`) and renders an Anthropic/OpenAI-shaped JSON envelope with `Content-Type: application/json` plus an `x-request-id` header. But these two layers bypass it entirely: `TimeoutLayer` returns the configured 408 status with an **empty body** (its bundled test `timeout_response_has_empty_body` asserts this); `DefaultBodyLimit`'s oversized-body path produces `BytesRejection::FailedToBufferBody` → `LengthLimitError` rendered via `define_rejection!` as a 413 with a **plain-text** body (`text/plain; charset=utf-8`, "Failed to buffer the request body"). Neither carries an `x-request-id` (that header is attached inside the pipeline/handlers at `core_pipeline.rs:512-513, 727-728`, not by any outer global layer). So a 408/413 is the one place in the API where the error schema is inconsistent — a real client-facing schema break and observability gap.
- **Recommendation:** Wrap both layers in a `HandleErrorLayer` that maps the rejection into `route_error_response(...)`, reusing the `not_found` path-prefix heuristic (`/v1/chat/completions` → OpenAiChat, else Anthropic) for protocol selection since the layer sits above routing. At minimum the 408/413 should carry the JSON envelope and an `x-request-id`.
- **Verifier note:** Load-bearing claim (both layers bypass `route_error_response`, non-JSON bodies, no `x-request-id`) verified against tower-http/axum-core source. Two minor body-content mischaracterizations in the original candidate (TimeoutLayer does not emit a "Request timeout" string — the body is empty; the 413 message is "Failed to buffer the request body", not tower-http's "length limit exceeded") do not change the verdict. Not a duplicate: first-pass HIGH-2 is the zero-duration config footgun, not the body-schema break. MEDIUM: affects only the 408/413 paths but is a genuine client-facing schema inconsistency.

---

### LOW-1 — `ProxyRequest.stream` bool is dead weight that the transport cannot enforce

- **File:** `crates/llm-proxy-provider/src/transport.rs:62-81`
- **Dimension:** Apollo / type-state
- **Evidence:** the field's own doc comment states it is **NOT read by `ProxyClient`**; callers must pick `send` vs `send_stream`.
- **Why it matters (softened):** The HIGH/"eliminates wrong-method bugs" framing in the original candidate was **not supported** by the call graph — the two production call sites (`core_pipeline.rs:466` and `:610`) are statically determined by which handler Axum routed into; no runtime `if req.stream` dispatch exists. The field is genuinely populated (anthropic.rs:743, openai_chat.rs:610, gemini.rs:489, responses.rs:602) and used by the `Debug` impl for logging. So this is a legitimate but **low-impact** design refinement: turn a documented invariant into a compile-time one.
- **Recommendation:** Split into `ProxyRequest` (non-streaming, valid with `send`) and `StreamingProxyRequest` (valid with `send_stream`), or use `ProxyRequest<NonStreaming>` / `ProxyRequest<Streaming>` via `PhantomData<Mode>`. Keep the field on the streaming variant for logging.
- **Verifier note:** Severity adjusted HIGH → LOW; no actual dispatch bug class exists in the current call graph.

---

### LOW-2 — Restriction-lint `#[allow]`s suppress lints that are never enabled (inert annotations)

- **File:** `crates/llm-proxy-provider/src/transport.rs:128`; `crates/llm-proxy-provider/src/adapter/anthropic.rs:42, 56`; `crates/llm-proxy-server/src/lib.rs:33`
- **Dimension:** Apollo / clippy-linting
- **Evidence:** `#[allow(clippy::expect_used)]` on `.expect(...)` calls, and `#[allow(clippy::double_must_use)]`. `[workspace.lints.clippy]` only sets `all = warn`; these restriction lints are off, so the `#[allow]`s currently suppress nothing.
- **Why it matters (softened):** Pure lint hygiene. **Important correction to the candidate's reasoning:** naively converting the three `expect_used` `#[allow]`s to `#[expect(clippy::expect_used)]` would **not** emit an "unfulfilled expectation" warning — it activates the lint locally and emits the real `used expect() on Ok value` warning, introducing new diagnostics. Only the `double_must_use` site (lib.rs:33) behaves as "unfulfilled".
- **Recommendation:** Either enable the restriction lints in workspace config (making the `#[allow]`s meaningful guards), or convert to `#[expect(...)]` understanding the `expect_used` ones will then surface real diagnostics that must be addressed. The `#[allow]`s are intentional documented suppressions, not dead code.
- **Verifier note:** Confirmed inert under current config; verified the `#[expect]` mechanism behaves differently than the candidate claimed for `expect_used`.

---

### LOW-3 — `#[allow(deprecated)]` on live code should be `#[expect(deprecated)]`

- **File:** `crates/llm-proxy-protocol/src/anthropic.rs:211, 333, 506, 635, 873`
- **Dimension:** Apollo / clippy-linting
- **Evidence:** `#[allow(deprecated)] // constructs ContentBlock with output: None` on `pub fn content_blocks`; the `output` field (line 307-309) is `#[deprecated(since = "0.1.0")]`.
- **Why it matters:** These four production sites (211, 333, 506, 635; 873 is test) genuinely touch the deprecated field. `#[allow]` silently becomes a no-op if the field is later removed; `#[expect(deprecated)]` emits `unfulfilled_lint_expectations` to surface the stale suppression. `rust-version = "1.85"` is set, so `expect` (stable since 1.81) is available.
- **Recommendation:** Convert the four production `#[allow(deprecated)]` sites to `#[expect(deprecated)]` with the existing reason comment.
- **Verifier note:** Test-site 873 is more defensible as `allow`, but the finding's recommendation still technically applies.

---

### LOW-4 — `model_allowed` re-evaluates `glob_matches` redundantly

- **File:** `crates/llm-proxy-core/src/catalog.rs:71-78`
- **Dimension:** Apollo / performance
- **Evidence:** `allow.iter().any(glob_matches)` computed for `allowed`, then `allow.iter().any(... glob_matches ...)` again for `explicitly_allowed`.
- **Why it matters (softened):** At most 2 evaluations per `allow` pattern (not 3 as the candidate claimed), plus 1 per `deny` pattern. `glob_matches` takes the `GLOB_CACHE` mutex + linear scan, so the redundant pass is not free. However: (a) the common `allow = ["*"]` case is **already** short-circuited (pass 3 guards on `pattern != "*"`); (b) `model_allowed` runs only inside `merge_catalog` (catalog build/refresh, bounded by `cache_ttl`), **not** per HTTP request; (c) `allow`/`deny` are typically 1-3 config entries and regex compilation is already cached.
- **Recommendation:** Compute `let matched: Vec<bool> = allow.iter().map(|p| glob_matches(p, model)).collect()` once, derive both `allowed` and `explicitly_allowed` from it.
- **Verifier note:** Severity MEDIUM → LOW; cool path, small inputs, common case already optimal.

---

### LOW-5 — `resolve_provider_route` clones the entire headers `HashMap` per request

- **File:** `crates/llm-proxy-core/src/provider_registry.rs:392-402`
- **Dimension:** Apollo / performance
- **Evidence:** builds a fresh `ProviderAdapterTargetConfig` each call, deep-cloning `adapter_cfg.headers` (`HashMap<String,String>`) plus `protocol`, `endpoint`, `provider_name`, etc.
- **Why it matters:** `resolve_target` (`core_pipeline.rs:323-324`) runs on every inbound request. The headers map is only ever iterated by reference in `transport.rs:161, 201` — never mutated — so the owned `HashMap` is unnecessary.
- **Recommendation:** Wrap `headers` (ideally the whole `ProviderAdapterTargetConfig`) in `Arc` so per-request resolution is a refcount bump.
- **Verifier note:** Severity MEDIUM → LOW; adapter headers are tiny (typically 1-3 entries), and the clone cost is negligible next to the upstream HTTP round-trip.

---

### LOW-6 — `RateLimiter` double HashMap lookup and unconditional IP `String` allocation

- **File:** `crates/llm-proxy-server/src/middleware.rs:253-259`
- **Dimension:** Apollo / performance
- **Evidence:** `!buckets.contains_key(client_ip)` (hash+probe), then `buckets.entry(client_ip.to_owned())` (hash+probe again, with unconditional `to_owned()` even on the Occupied arm).
- **Why it matters:** `is_allowed` runs on every non-loopback request (`core_pipeline.rs:238`). On the hot path (existing IP, under capacity) the `contains_key` is purely redundant, and `to_owned()` allocates a `String` immediately dropped when the key already exists.
- **Recommendation:** Use the `entry` API directly with `Occupied`/`Vacant` match (single lookup, no redundant `contains_key`). **Correction:** the candidate's `HashMap<IpAddr, _>` re-keying is inaccurate — `get_client_ip` (lines 353-388) returns the `"unknown"` sentinel for unidentifiable clients, which is not a parseable `IpAddr`, so a pure `IpAddr` rekey would drop the fallback bucket.
- **Verifier note:** Severity MEDIUM → LOW; mutex-guarded path, dominant cost is upstream HTTP I/O; one sub-claim (`IpAddr` rekey) is invalid.

---

### LOW-7 — `Metrics::record_success` allocates a `format!` key `String` on every successful request

- **File:** `crates/llm-proxy-core/src/metrics.rs:95-98`
- **Dimension:** Apollo / performance
- **Evidence:** `let key = format!("{provider}{KEY_SEPARATOR}{model}"); ... map.entry(key).or_insert(0) += 1;`
- **Why it matters:** Called at `core_pipeline.rs:483` (non-streaming success) and `:1073` (streaming success) — the genuine per-request success path. `format!` heap-allocates a fresh `String` every call, and `entry(key)` consumes it even when the key already exists (the allocation is wasted in the common already-tracked case).
- **Recommendation:** Use a `thread_local!` reusable buffer with `write!`, or key the map on `(Arc<str>, Arc<str>)` / an interned struct so no formatting happens.
- **Verifier note:** Severity MEDIUM → LOW; single ~20-40 byte allocation on a path dominated by network I/O and JSON (de)serialization, already behind a `Mutex`.

---

### LOW-8 — `RequestDeduplicator::hash_path_body` allocates a hex `String` per request for the HashMap key

- **File:** `crates/llm-proxy-server/src/middleware.rs:123-128`
- **Dimension:** Apollo / performance
- **Evidence:** `format!("{:x}", hasher.finalize())` produces a 64-char `String` used solely as the key for `in_flight: Mutex<HashMap<String, Instant>>`.
- **Why it matters:** Runs on every request when dedup is enabled (and body under the 64 KiB cap). The 32-byte SHA-256 digest is `Copy + Eq + Hash`, so the hex `String` allocation and formatting pass are entirely avoidable.
- **Recommendation:** Use `HashMap<[u8; 32], Instant>` keyed on the raw digest.
- **Verifier note:** Severity MEDIUM → LOW; dedup defaults to disabled (opt-in), the allocation is small, and the SHA-256 computation over up to 64 KiB of body dominates.

---

### LOW-9 — `catalog_contains_model` does a linear scan on every catalog-enforced request

- **File:** `crates/llm-proxy-server/src/routes/core_pipeline.rs:397-412`
- **Dimension:** Apollo / performance
- **Evidence:** `catalog.iter().any(|model| model.id == upstream_model)` (plus a second linear scan for the gemini path, lines 406-408), invoked at line 340 inside `resolve_target` when `provider.catalog.enforce` is true.
- **Why it matters (softened):** O(N) per catalog-enforced request, but (a) gated behind opt-in `enforce` (default false); (b) N is bounded per-provider (tens to a few hundred); (c) the scan is many orders of magnitude below the noise floor of the upstream HTTPS call it precedes. The freshly-merged catalog (cloned twice per `catalog()` call: `catalog_service.rs:65, 83`) has no persistent indexable structure.
- **Recommendation:** Build and cache a `HashSet<String>` (or `HashSet<&str>`) keyed by model id alongside the `Vec` in `ModelCatalogService`, turning the per-request check into O(1).
- **Verifier note:** Severity MEDIUM → LOW; micro-optimization, no practical perf effect under normal load.

---

### LOW-10 — Zero-duration `request_timeout` silently disables per-request timeout (config-side companion to HIGH-2)

- **File:** `crates/llm-proxy-server/src/state.rs:154-156` (test at `:581-593`)
- **Dimension:** Axum / production-shutdown (unbounded-config-input)
- **Evidence:** `request_timeout()` is a plain passthrough accessor; the test asserts `Duration::ZERO` round-trips through `AppState::new` unchanged.
- **Why it matters:** Same root cause as HIGH-2, viewed from the config/state side. `load_app_config` rejects nonsensical `server.hot_reload=true` but leaves `request_timeout` unvalidated — an inconsistency rather than an intentional exception.
- **Recommendation:** Clamp/reject values below ~1s during `ServerConfig` validation.
- **Verifier note:** Severity kept LOW (the DoS requires operator misconfiguration, not external trigger; failure is loud, every API request 408s). See HIGH-2 for the full mechanism.

---

### LOW-11 — No `CatchPanicLayer`: a handler panic drops the connection without a 500

- **File:** `crates/llm-proxy-server/Cargo.toml:21`; `crates/llm-proxy-server/src/routes/mod.rs:67-91`
- **Dimension:** Axum / production-shutdown
- **Evidence:** `tower-http = { version = "0.6", features = ["trace", "timeout"] }` — `catch-panic` not enabled; workspace grep for `CatchPanic|catch_unwind|set_hook` returns zero hits.
- **Why it matters (softened):** In axum 0.8 a handler panic tears down the in-flight connection (client sees a reset, not a structured 500) and bypasses `TraceLayer`'s response-span logging. However, a scan of actual handlers shows **no current runtime panic sources** — the `unwrap()`/`expect()`/`panic!` occurrences (`core_pipeline.rs:270, 1239`, `error_response.rs:390`) are compile-time-constant regex compilations or `#[cfg(test)]`. Defense-in-depth against future/dependency panics.
- **Recommendation:** Enable `catch-panic`, insert `CatchPanicLayer::new()` as the outermost layer of the merged `Router`.
- **Verifier note:** Severity MEDIUM → LOW; no present exploitable defect.

---

### LOW-12 — No security headers (`X-Content-Type-Options` / explicit CORS)

- **File:** `crates/llm-proxy-server/src/routes/mod.rs:67-91`
- **Dimension:** Axum / production-shutdown
- **Evidence:** router applies only `DefaultBodyLimit`, `TraceLayer`, `TimeoutLayer`; grep for `SetResponseHeader|CorsLayer|nosniff|cors` returns zero matches; the `set-header`/`cors` tower-http features are not even enabled.
- **Why it matters (softened):** JSON error/response bodies are returned with no MIME-hardening and no explicit cross-origin policy. Practical exposure is low because this is a server-to-server proxy (SDK clients, no cookie/session auth).
- **Recommendation:** Add `SetResponseHeaderLayer` for `X-Content-Type-Options: nosniff` on all responses; add a `CorsLayer` with an explicit `allow_origin` list (or a per-config flag) if browser clients are ever supported.
- **Verifier note:** Severity MEDIUM → LOW; same-origin default is reasonable for the common case.

---

### LOW-13 — Health endpoint exposes live operational metrics on an unauthenticated route

- **File:** `crates/llm-proxy-server/src/routes/health.rs:37-56` (mounted unauthenticated at `routes/mod.rs:71-75`)
- **Dimension:** Axum / production-shutdown
- **Evidence:** `health` returns `HealthMetrics { requests_received, requests_streamed, requests_success, requests_failed, upstream_calls, rate_limited, deduplicated, ... }` on the `lightweight` router with only `TraceLayer`. The doc note confirms the route is unauthenticated.
- **Why it matters (softened):** Aggregate success/failure/rate-limited counts are returned to any caller. The authors already omit the more sensitive per-provider/per-model counters deliberately. The cited rule ("health should be lightweight") is technically satisfied — the handler does no I/O and returns instantly via a single in-memory snapshot.
- **Recommendation:** Keep `/health` trivial (`status: ok`) for liveness probes; move the metrics snapshot to a separate authenticated `/admin/metrics` (or `/ready`) gated by an auth layer.
- **Verifier note:** Severity kept LOW (arguably INFO); deliberate, documented tradeoff; aggregate data only.

---

### LOW-14 — Adapter helpers take `&Vec<T>` instead of `&[T]`

- **File:** `crates/llm-proxy-provider/src/adapter/responses.rs:335`
- **Dimension:** Apollo / idioms-ownership (owned-vs-borrowed-params)
- **Evidence:**
  ```rust
  fn infer_stop_reason(&self, outputs: Option<&Vec<ResponsesOutput>>) -> StopReason {
  ```
- **Why it matters:** `&Vec<T>` forces callers to keep a concrete `Vec` and defeats slice coercion; `&[T]` is the idiomatic, more general signature (chapter_01 §1.1: "Prefer `&[T]` instead of `Vec<T>` or `&Vec<T>`"). The body only calls `outs.iter().any(...)`, which works unchanged on a slice — a textbook `clippy::ptr_arg` gap. Call sites pass `chunk.output.as_ref()` (`chunk.output: Option<Vec<ResponsesOutput>>`), which would cleanly switch to `.as_deref()`. No `#[allow(clippy::ptr_arg)]` suppression.
- **Recommendation:** Change to `Option<&[ResponsesOutput]>`.
- **Verifier note:** Primary cited location is genuine and accurate. **One part of the original recommendation is wrong and dropped:** the candidate also proposed changing `close_tool_blocks(&mut self, events: &mut Vec<CoreEvent>)` at `openai_chat.rs:311` to a slice form — but the body calls `events.push(...)`, and `&mut [T]` has no `push`; `&mut Vec<T>` is the correct signature there. Only the `responses.rs:335` half applies. Not a duplicate of audit-report #101 (that targets a different perf issue at `openai_chat.rs:307`).

---

### LOW-15 — `merge_catalog` clones each model id twice (BTreeMap key + cloned entry value)

- **File:** `crates/llm-proxy-core/src/catalog.rs:51` (also `:59` static branch)
- **Dimension:** Apollo / idioms-ownership (redundant-clone)
- **Evidence:**
  ```rust
  for model in discovered {
      entries.insert(model.id.clone(), model.clone());
  }
  ```
- **Why it matters:** The id `String` is cloned for the map key **and** again inside `model.clone()` (the value retains its own `id` field — `StaticModelCatalogEntry.id: String` at `provider_config.rs:378`). The map keys are then immediately discarded by `into_values()` at line 64, so the key clone is a strictly surplus allocation of the same short id text. Both the discovered branch (line 51) and the static branch (line 59) have the same double-clone. The per-entry clone is unavoidable (`merge_catalog` borrows both inputs), but the *second* id clone (the `BTreeMap` key) is genuinely redundant.
- **Recommendation:** Restructure so the id is derived once — e.g., key on a separate id `String` and accept the value's id, or build the `Vec` directly without the intermediate keyed map. At minimum the static branch can be tightened.
- **Verifier note:** Confirmed verbatim; the key clone is genuinely redundant. Cold catalog-build/refresh path (gated by `cache_ttl`, not per-request), N bounded, short id strings — hence LOW. The recommendation is slightly imprecise (you cannot borrow the key from the value you are simultaneously inserting) but the core observation is fixable.

---

### LOW-16 — `models` route clones first/last id then re-clones every id in the map, despite a comment claiming it avoids the clone

- **File:** `crates/llm-proxy-server/src/routes/models.rs:126` (clone sites `:126-127`; re-clone `:129-149`)
- **Dimension:** Apollo / idioms-ownership (redundant-clone)
- **Evidence:**
  ```rust
  // Extract first/last IDs from entries (before building ModelCards) to
  // avoid cloning from the already-owned Vec.
  let first_id = entries.first().map(|m| m.id.clone());
  let last_id = entries.last().map(|m| m.id.clone());
  ...
  .map(|entry| ModelCard { id: entry.id.clone(), ...
  ```
- **Why it matters:** `entries` is fully owned (`catalog()` returns `Vec<StaticModelCatalogEntry>` by value). The subsequent `.iter().map(|entry| ModelCard { id: entry.id.clone(), ... })` re-clones **all** ids — including re-cloning the first and last — so the early extraction does not actually avoid the later clone; the comment contradicts itself. The extraction only reorders when the clone happens, it does not eliminate it.
- **Recommendation:** Since `entries` is owned, consume it with `into_iter()` so `entry.id` moves into each `ModelCard` with zero clones; derive `first_id`/`last_id` from the owned `Vec` via `.first()`/`.last()` **before** the `into_iter()` move. This removes N id clones and makes the comment accurate.
- **Verifier note:** Technically accurate. Overlap: audit-report #305 already flagged the same first/last clone; this finding adds the angle that the code comment contradicts itself. LOW — a handful of `String` clones on a non-hot models-listing path.

---

### LOW-17 — Generated launchd plist and desktop entry tested with fragile `contains()` instead of insta snapshots

- **File:** `apps/llm-proxy/src/platform.rs:184` (`fn format_plist_basic`)
- **Dimension:** Apollo / testing (missing-snapshot-test)
- **Evidence:**
  ```rust
  fn format_plist_basic() {
      ...
      let plist = format_plist(&args);
      assert!(plist.contains("<string>/usr/bin/llm-proxy</string>"));
      assert!(plist.contains("<string>serve</string>"));
      assert!(plist.contains("<?xml version=\"1.0\""));
      assert!(plist.contains("com.llm-proxy"));
  }
  ```
- **Why it matters:** `format_plist` emits a ~20-line XML document and `format_desktop_entry` emits a multi-line INI document; per chapter_05 §5.5 these rendered-output generators are textbook insta candidates, and substring checks silently miss structural regressions (e.g. a dropped `<key>KeepAlive</key>`). `insta` is not a dependency anywhere in the workspace (verified: no `insta::` usage, no `insta =` in any `Cargo.toml`).
- **Recommendation:** Replace the four `contains()` checks with `insta::assert_snapshot!("launchd_plist/basic", format_plist(&args))` (redacting the platform-dependent log path with a `{{log_path}}` selector). Add `insta = { version = "1.42", features = ["yaml"] }` as a dev-dependency.
- **Verifier note:** Code is verbatim. Reduced MEDIUM → LOW: (1) this is a test-quality improvement, not a defect — the existing `contains()` assertions still catch the regressions they were written for; (2) two complementary structural tests (`format_plist_escapes_special_chars` line 198, `format_plist_malformed_path` line 215) substantially mitigate the "silently miss" risk; (3) the recommendation itself acknowledges the platform-dependent log path makes the snapshot non-trivial.

---

### LOW-18 — SSE stream integration tests assert 5+ unrelated behaviors in one test

- **File:** `crates/llm-proxy-server/tests/core_pipeline.rs:791` (also `:850`, `:898`, `:946`)
- **Dimension:** Apollo / testing (multi-behavior-test)
- **Evidence:**
  ```rust
  async fn stream_anthropic_provider_returns_sse_text_deltas() {
      ...
      assert_eq!(resp.status(), StatusCode::OK);
      ... assert!(ct.contains("text/event-stream"), ...);
      ... assert!(request_id.is_some(), "x-request-id must be present...");
      ... assert!(text.contains("event: message_start"), ...);
      ... assert!(text.contains("event: content_block_delta"), ...);
      ... assert!(text.contains("event: message_stop"), ...);
      ... assert!(text.contains("Hi!"), ...);
  }
  ```
- **Why it matters:** This one test asserts HTTP status, SSE content-type header, request-id presence, three distinct SSE event types, **and** translated text content (7 assertions). Per chapter_05 §5.1 ("only test one behavior per function") and §5.4 ("ideally one assertion per test"), a failure in the earliest `assert` hides all later checks. The same pattern repeats at lines 850, 898, and 946.
- **Recommendation:** Split into focused tests, or snapshot the full SSE frame sequence with `insta::assert_snapshot!` after redacting volatile fields.
- **Verifier note:** Code verbatim; 7 assertions span HTTP envelope, headers, SSE protocol, and content. Reduced MEDIUM → LOW: no production impact, only test-diagnostic granularity; and the "one assertion per test" rule is weakest for integration tests of a single end-to-end streaming transaction (splitting re-spawns the mock server and rebuilds the router per test). Not a duplicate.

---

### LOW-19 — `transport.rs` test server uses fixed 50ms sleep for readiness; drop-stream test uses 200ms sleep with a timing-dependent `count < 50` assertion

- **File:** `crates/llm-proxy-provider/src/transport.rs:436` (readiness sleep); `:915` (drop-stream sleep), `:920` (`count < 50` assertion)
- **Dimension:** Apollo / testing (flaky-timing-test)
- **Evidence:**
  ```rust
  // start_test_server (line 426-438)
  // Give the server a moment to start accepting connections.
      tokio::time::sleep(Duration::from_millis(50)).await;
  ...
  // dropping_stream_aborts_upstream (line 915-920)
  // Wait a bit for the server to notice the disconnect.
      tokio::time::sleep(Duration::from_millis(200)).await;
      ... assert!(count < 50, "Dropping the stream should abort upstream; counter was {}", count);
  ```
- **Why it matters:** Two flaky-timing patterns. (1) `start_test_server` sleeps 50ms and hopes the server is ready — under CI load the first request can hit connection-refused. The workspace already contains the exact replacement: `wait_for_ready(addr)` helpers at `crates/llm-proxy-server/tests/chat_completions.rs:27-46` and `tests/core_pipeline.rs:32` (their doc comments say "Replaces `tokio::time::sleep(Duration::from_millis(50))` with a deterministic readiness check"). `transport.rs` uses the inferior variant while backing ~19 tests. (2) `dropping_stream_aborts_upstream` sleeps 200ms then asserts `count < 50`, but abort propagation is nondeterministic — on a saturated single-threaded runner the counter can legitimately exceed 50.
- **Recommendation:** (1) Replace the fixed 50ms with the port-poll `wait_for_ready` helper already used in the `tests/` tree. (2) Relax to the deterministic property (`count < 100`, i.e. "did not run to completion") or poll the counter until it stabilizes before asserting.
- **Verifier note:** Both patterns confirmed. **Cite correction:** the readiness helper actually lives in `tests/chat_completions.rs:27-46` and `tests/core_pipeline.rs:32`, not `src/routes/chat_completions.rs:37` — the recommendation still holds because the helper demonstrably exists. Both are `#[cfg(test)]` test-only. LOW: latent-flakiness nit, no record of actual flakes.

---

### LOW-20 — `is_toml_config_various` is a blob test and the same `validate_toml_extension` cases are duplicated across 3 test files

- **File:** `apps/llm-proxy/src/config_validation.rs:62` (`is_toml_config_various`); also duplicated at `apps/llm-proxy/tests/cli_validate.rs:64`; `tests/cli_serve.rs:9-39`
- **Dimension:** Apollo / testing (multi-behavior-test / duplication)
- **Evidence:**
  ```rust
  // config_validation.rs:62
  fn is_toml_config_various() {
      assert!(is_toml_config(Path::new("a.toml")));
      assert!(is_toml_config(Path::new("a.TOML")));
      assert!(!is_toml_config(Path::new("a.json")));
      assert!(!is_toml_config(Path::new("a")));
      assert!(!is_toml_config(Path::new("a.toml.bak")));
  }
  // SAME logic re-tested in cli_serve.rs (5 tests) and cli_validate.rs (5 tests)
  ```
- **Why it matters:** Two issues. (1) `is_toml_config_various` bundles five distinct input cases into one test with five assertions (chapter_05 §5.1) — and the finding understates it: the identical blob is duplicated verbatim at `cli_validate.rs:64-70`. (2) The `validate_toml_extension` accept/reject matrix is tested in **three** places: `config_validation.rs` unit tests, `cli_serve.rs` (5 tests whose header admits they only exercise pre-flight validation "without actually starting the server"), and `cli_validate.rs`.
- **Recommendation:** Split the blob into per-case tests (or `rstest` `#[case]` named cases). Keep the unit tests in `config_validation.rs` and remove the redundant copies in the CLI integration files, or collapse to a single shared `rstest` fixture.
- **Verifier note:** Both claims verified; the duplication is real. LOW: test-organization/style with zero functional impact.

---

### LOW-21 — Zero executable rust doc-tests; rich public API documented only with non-rust fenced blocks

- **File:** `crates/llm-proxy-core/src/catalog.rs:34` (also `catalog.rs:71` `model_allowed`, `provider_config.rs:663` `validate_provider_config`)
- **Dimension:** Apollo / testing (missing-doc-test)
- **Evidence:** A workspace-wide grep for ` ```rust` / ` ```no_run` / ` ```ignore` / ` ```should_panic` / ` ```compile_fail` returns **zero** hits. All 24 fenced doc blocks are ` ```json` / ` ```toml` / ` ```text` (non-executable, skipped by `cargo test --doc`). `pub fn parse_catalog_file`, `pub fn model_allowed`, `pub fn validate_provider_config` have one-line `///` descriptions and no doc example.
- **Why it matters:** chapter_05 §5.2 promotes doc examples as executable, always-compiled documentation that doubles as correctness checks. Key pure functions with clear contracts — `parse_catalog_file`, `model_allowed` (allow/deny glob logic), `validate_toml_extension`, `glob_matches` — are ideal for a one-line `///` rust example that runs under `cargo test --doc` and stays current via the compiler.
- **Recommendation:** Start with `model_allowed`: `/// assert!(model_allowed("gpt-4", &["gpt-*".into()], &[]));` inside a `/// ` ```ignore` block. Costs nothing and adds a doc-test layer the crate currently lacks entirely.
- **Verifier note:** Verified; the cited functions are pure and already exercised by `#[cfg(test)]` unit tests, making them ideal doc-test candidates. Not a duplicate. LOW — documentation/testing-hygiene, not correctness or security.

---

### LOW-22 — Broken intra-doc links: `adapter/mod.rs` module docs reference `CoreRequest`/`CoreResponse`/`CoreEvent` that do not resolve

- **File:** `crates/llm-proxy-provider/src/adapter/mod.rs:1` (links at `:1, :10, :11`)
- **Dimension:** Apollo / comments-docs (broken-intra-doc-links)
- **Evidence:**
  ```rust
  //! Provider protocol adapters: [`CoreRequest`] -> provider wire format and back.
  ...
  //! Adapters translate between the normalized core types ([`CoreRequest`],
  //! [`CoreResponse`], [`CoreEvent`]) and the provider-specific wire types.
  ```
- **Why it matters:** `cargo doc --no-deps -p llm-proxy-provider` emits exactly 4 `warning: unresolved link` diagnostics here. The types are imported via `use llm_proxy_protocol::core::{CoreEvent, CoreRequest, CoreResponse, ...}` (line 28) but that private `use` does not give rustdoc a linkable path — rustdoc resolves intra-doc links against the module's public surface, and a private `use` is excluded.
- **Recommendation:** Use fully-qualified links, e.g. `[`llm_proxy_protocol::core::CoreRequest`]`, or add `pub use llm_proxy_protocol::core::{CoreRequest, CoreResponse, CoreEvent};` re-exports and link to those.
- **Verifier note:** Verified empirically — `cargo doc` emits exactly 4 matching warnings. Confirmed not a duplicate: neither prior report mentions broken intra-doc links at this location.

---

### LOW-23 — Doc comment links to private/inaccessible constants `CHARS_PER_TOKEN` and `PER_MESSAGE_OVERHEAD` in token counter

- **File:** `crates/llm-proxy-core/src/token/counter.rs:38` (`CHARS_PER_TOKEN` private-intra-doc at `:38, :52`), `:75` (`PER_MESSAGE_OVERHEAD` unresolved at `:75`, declared function-local at `:84-86`)
- **Dimension:** Apollo / comments-docs (broken-intra-doc-links)
- **Evidence:**
  ```rust
  /// Uses a rough heuristic of [`CHARS_PER_TOKEN`] characters per token rather   // line 38 (private_intra_doc_links)
  ...
  /// The extra [`PER_MESSAGE_OVERHEAD`] per message accounts for formatting       // line 75 (unresolved link)
  ```
- **Why it matters:** `cargo doc` warns: `[`CHARS_PER_TOKEN`]` is a private `const` (line 34: `const CHARS_PER_TOKEN: usize = 4;`, no `pub`) so it trips rustdoc's warn-by-default `private_intra_doc_links`; `PER_MESSAGE_OVERHEAD`/`BASE_TOKENS`/`SYSTEM_OVERHEAD` are declared **inside** the `count_messages` function body (lines 84-86) so they are local items invisible to the doc comment above the function.
- **Recommendation:** Either (a) hoist these constants to module scope as `pub(crate)` and link them, or (b) drop the link brackets and render them as plain inline code `` `CHARS_PER_TOKEN` `` so the doc reads correctly without broken links.
- **Verifier note:** Both violations reproduce verbatim under `cargo doc`. Minor inaccuracy: the candidate said "3 warnings" for this file; it actually emits 3 relevant warnings here (two `CHARS_PER_TOKEN`, one `PER_MESSAGE_OVERHEAD`) plus 2 unrelated warnings in other files, for 5 total. Confirmed not a duplicate. LOW — purely cosmetic broken rendered-doc links.

---

### LOW-24 — Module doc link `[`interpolate_env_vars`]` does not resolve in `env_interpolate.rs`

- **File:** `crates/llm-proxy-core/src/env_interpolate.rs:3`
- **Dimension:** Apollo / comments-docs (broken-intra-doc-links)
- **Evidence:**
  ```rust
  //! Provides a single [`interpolate_env_vars`] function and the compiled regex
  ```
- **Why it matters:** `cargo doc -p llm-proxy-core --no-deps` emits `warning: unresolved link to interpolate_env_vars` / `no item named interpolate_env_vars in scope`. The function exists at line 44 (`pub fn interpolate_env_vars`), but the unqualified item link in the module-level `//!` comment does not resolve.
- **Recommendation:** Use a fully-qualified link `[`crate::env_interpolate::interpolate_env_vars`]`.
- **Verifier note:** Verified empirically. The candidate's causal explanation (module docs cannot forward-reference) is slightly imprecise, but the warning is real and the qualified-link fix is correct. Not a duplicate. LOW — documentation-only.

---

### LOW-25 — Doc comment link `[`Ordering::Relaxed`]` in `metrics.rs` module doc does not resolve

- **File:** `crates/llm-proxy-core/src/metrics.rs:9`
- **Dimension:** Apollo / comments-docs (broken-intra-doc-links)
- **Evidence:**
  ```rust
  //! All atomic operations use [`Ordering::Relaxed`] which is correct here
  ```
- **Why it matters:** `cargo doc -p llm-proxy-core --no-deps` warns `unresolved link to Ordering::Relaxed` / `no item named Ordering in scope` on line 9. Rustdoc's intra-doc-link resolver does not reliably resolve path-style links like `[Ordering::Relaxed]` in `//!` comments against module-level `use` imports.
- **Recommendation:** Use a fully-qualified link `[`std::sync::atomic::Ordering::Relaxed`]`. (Adding another `use std::sync::atomic::Ordering;` would not reliably fix it — one already exists at line 16 and the warning still fires.)
- **Verifier note:** Verified by running `cargo doc` — the exact claimed warning fires on line 9. The candidate's explanation is slightly inaccurate (it claims `Ordering` is not imported, but it is, at line 16) — this is a genuine rustdoc resolution quirk, context-dependent and brittle. The fully-qualified link is the correct fix. Not a duplicate. LOW — documentation-only.

---

### LOW-26 — Doc comment link `[`is_duplicate_with_path`]` on deprecated method does not resolve

- **File:** `crates/llm-proxy-server/src/middleware.rs:80`
- **Dimension:** Apollo / comments-docs (broken-intra-doc-links)
- **Evidence:**
  ```rust
  /// Prefer [`is_duplicate_with_path`] which includes the request path in the
  /// hash to distinguish requests to different protocol endpoints.
  ```
- **Why it matters:** `cargo doc --no-deps --document-private-items` warns `unresolved link to 'is_duplicate_with_path'` at `middleware.rs:80`. The target method is defined on the same `impl RequestDeduplicator` block at line 92, but rustdoc's intra-doc-link resolver does not search sibling inherent methods by bare name — it requires a `Self::` or type-qualified path.
- **Recommendation:** Change to `[`Self::is_duplicate_with_path`]` or `[`RequestDeduplicator::is_duplicate_with_path`]`.
- **Verifier note:** Verified empirically. Distinct from prior reports: they mention `is_duplicate`/`is_duplicate_with_path` only in the context of the deprecated-method clippy lint (MEDIUM-3) and SHA-256 hashing, not the broken doc link. LOW — documentation-only.

---

### LOW-27 — Bare axum 405 bypasses the protocol-aware fallback; method-not-allowed responses are empty instead of OpenAI/Anthropic-shaped

- **File:** `crates/llm-proxy-server/src/routes/mod.rs:94-103`
- **Dimension:** Axum / router-routing (method-routing)
- **Evidence:**
  ```rust
  .route("/providers/{provider}/v1/messages", post(handle_messages))
  .route("/providers/{provider}/v1/messages/count_tokens", post(count_tokens))
  .route("/providers/{provider}/v1/chat/completions", post(handle_chat_completions))
  .route("/providers/{provider}/v1/models", get(handle_models))
  ```
- **Why it matters:** A GET (or PUT/DELETE) to an existing POST-only route does **not** hit the carefully-built `not_found` fallback (`routes/mod.rs:109` only handles unmatched paths). Instead axum 0.8's per-route `MethodRouter` returns a stock `405 Method Not Allowed` with an empty body that is **not** shaped by `error_response::route_error_response`, so the client gets an inconsistent, non-protocol error. There is no `RouteError::MethodNotAllowed` variant (grep: zero `MethodNotAllowed` across the crate) and no `.method_not_allowed_fallback(...)` override. The existing tests (`tests/integration.rs:638-672`, `tests/core_pipeline.rs:1716-1724`) assert only `resp.status() == METHOD_NOT_ALLOWED`, never the body — confirming the body is unshaped.
- **Recommendation:** Add a `RouteError::MethodNotAllowed` variant and wire it via axum 0.8's `Router::method_not_allowed_fallback(...)`, delegating to `route_error_response` (reusing the `not_found` path-prefix heuristic for protocol selection).
- **Verifier note:** Three-way verification (no `MethodNotAllowed` variant; no `method_not_allowed_fallback`; tests assert status only). Reduced MEDIUM → LOW: the status code is correct and HTTP-compliant; only the body is empty, and wrong-method probes to POST-only chat/messages endpoints are an uncommon edge case. The cited SKILL.md sections do not actually prescribe protocol-shaping of 405s, so this is a consistency gap rather than a documented-rule violation.

---

### LOW-28 — Handlers manually parse JSON with `serde_json::from_slice`, discarding `JsonRejection` diagnostics

- **File:** `crates/llm-proxy-server/src/routes/messages.rs:64` (also `chat.rs:67`, `token_count.rs:72`)
- **Dimension:** Axum / handlers-extractors (extractor-redundancy)
- **Evidence:**
  ```rust
  let req: MessageRequest = serde_json::from_slice(&body)
      .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;
  ```
- **Why it matters:** The body is already extracted as `Bytes` (necessary — `prepare_request` hashes it for dedup first). But after hashing, the parse should go through `Json::<MessageRequest>::from_bytes(&body)` so that `JsonRejection` variants (`JsonSyntaxError` vs `JsonDataError`) are distinguished and mapped to precise 400 bodies, matching the axum "Extractor Rejection Handling" pattern. Currently all three collapse syntax and data errors into one opaque "invalid JSON" string.
- **Recommendation:** Use `axum::Json::from_bytes(&body)` + match on `JsonSyntaxError`/`JsonDataError` to populate a distinct `error_type`.
- **Verifier note:** Code confirmed verbatim across all three handlers. Reduced MEDIUM → LOW for three reasons: (1) `MissingJsonContentType` is a header check performed only by `Json::from_request`, **not** `from_bytes` — the handler already extracted `Bytes`, so the content-type distinction is already bypassed regardless of the parse helper; only syntax-vs-data (2 categories, not 3) is genuinely collapsed. (2) The error message is `format!("invalid JSON: {e}")` — the serde `Error` Display **is** interpolated and is informative; only `error_type` granularity is lost. (3) The client still receives a correct structured 400 envelope. Related to audit-report #154 (the `Bytes`-vs-`Json` choice) but this finding is the actionable downstream consequence.

---

### LOW-29 — `chat.rs` and `messages.rs` run rate-limit and dedup before validating provider name or JSON, returning 429/409 for malformed requests

- **File:** `crates/llm-proxy-server/src/routes/messages.rs:55-65` (also `chat.rs:58-68`; correct ordering at `token_count.rs:65-73`)
- **Dimension:** Axum / error-handling-intoresponse (wrong-missing-http-status-code)
- **Evidence:**
  ```rust
  let ctx = core_pipeline::prepare_request(
      &state, &headers, connect_info.as_ref(), &body, &request_path,
  )?;
  // Parse and validate the Anthropic MessageRequest.
  let req: MessageRequest = serde_json::from_slice(&body)
      .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;
  ```
- **Why it matters:** `prepare_request` (`core_pipeline.rs:238-246`) charges rate-limit and dedup counters (returning `RouteError::RateLimited`/`Conflict`) **before** the JSON or provider name is ever validated. So a client POSTing garbage bytes to `/providers/{bad}/v1/messages` consumes rate-limit budget and (if the IP bucket is full or bytes are duplicated) returns a 429/409 instead of a 400 invalid-request, and every malformed probe burns rate-limit budget. `token_count.rs` does it correctly: `validate_provider_name` (line 65) **then** `prepare_request` (line 70).
- **Recommendation:** Reorder `chat.rs` and `messages.rs` to validate the provider name and parse the JSON **before** `prepare_request`, mirroring `token_count.rs`. Path-level provider-name validation is the cheapest input gate and should always precede rate accounting.
- **Verifier note:** Factual core verified across all three files. Reduced MEDIUM → LOW: the status codes are not "wrong" in the conventional sense — `RateLimited`/`Conflict` correctly map to 429/409 for the conditions that trigger them; the issue is a cheaper, more informative 400 input gate should precede rate accounting. Dedup defaults to disabled (opt-in), and a single malformed probe still returns 400 (after consuming one rate-limit unit). No crash, no injection. Recommendation (mirror `token_count.rs`) is correct.

---

### LOW-30 — POST handlers accept any Content-Type and attempt JSON parse, so non-JSON bodies report a misleading 'invalid JSON' 400

- **File:** `crates/llm-proxy-server/src/routes/chat.rs:67-68` (also `messages.rs:64-65`, `token_count.rs:72-73`)
- **Dimension:** Axum / error-handling-intoresponse (JsonRejection-not-handled-distinctly)
- **Evidence:**
  ```rust
  let req: ChatCompletionRequest = serde_json::from_slice(&body)
      .map_err(|e| RouteError::InvalidRequest(format!("invalid JSON: {e}")))?;
  ```
- **Why it matters:** Because the handlers take `axum::body::Bytes` (not the `Json` extractor), they never see a `JsonRejection` and never check `Content-Type`. A request with `Content-Type: text/plain` (or form-encoded data) that serde cannot parse gets the same "invalid JSON: ..." message as a genuinely malformed JSON body — misleading, because the body was never JSON and the real problem is the wrong content type.
- **Recommendation:** Add a `Content-Type` check at handler entry: if the request has a `Content-Type` header and it is not `application/json` (or `+json`), return `RouteError::InvalidRequest("expected application/json")` with a distinct message before attempting `serde_json::from_slice`. Apply uniformly to all three POST handlers.
- **Verifier note:** Code accurate; pattern repeated identically in all three handlers. **Caveat on the reference:** the skill's `JsonRejection` taxonomy (MissingJsonContentType/JsonDataError/JsonSyntaxError) only exists for the `Json<T>` extractor, which the proxy deliberately avoids (it needs raw bytes for the dedup hash). So the literal `JsonRejection` taxonomy is structurally inapplicable — the finding really appeals to the *spirit* of that guidance (distinct error for wrong-content-type vs malformed-JSON), which is valid. Reduced to LOW: pure error-message-UX; both cases already return a correct 400 with a descriptive message; only the wording is slightly off for one case.

---

### LOW-31 — Inconsistent log level for the same error path: `messages.rs`/`models.rs` use `info!`, `chat.rs` uses `warn!`

- **File:** `crates/llm-proxy-server/src/routes/messages.rs:39` (`info!`); `models.rs:99` (`info!`); `chat.rs:39` (`warn!`); `token_count.rs:53` (no log at all)
- **Dimension:** Axum / error-handling-intoresponse (inconsistent-error-body-schema)
- **Evidence:**
  ```rust
  // messages.rs:39
  info!(error = %error, "request failed");
  // models.rs:99
  info!(error = %error, "models request failed");
  // chat.rs:39
  warn!(error = %error, "request failed");
  ```
- **Why it matters:** All three handlers sit at the identical boundary (`Err(error)` → `route_error_response`) operating on the same `RouteError` type. `RouteError`'s variant set includes genuine server/upstream failures — `Upstream` (502), `UpstreamTimeout` (504), `ProviderDecode` (502), `Internal` (500) — failures an operator would expect at `warn!` or above. Yet the same condition surfaces as `warn` from `/chat/completions`, `info` from `/messages` and `/models`, and silence from `/count_tokens`. This is a genuine observability inconsistency: a 502/504 from one route is noisier than the same failure from another.
- **Recommendation:** Pick one level (`warn!` is defensible given the 5xx variants present) and apply it uniformly across all four handlers. A fully ideal fix would discriminate by status (client 4xx vs server/upstream 5xx) rather than blanket-applying one level.
- **Verifier note:** All four sites verified as claimed; `RouteError` variants confirmed. Not a duplicate. LOW: no functional impact, purely observability consistency.

---

### INFO-1 — Misleading comment claims `ProviderAdapter` clone is 'Arc-like'

- **File:** `crates/llm-proxy-server/src/routes/core_pipeline.rs:368`
- **Dimension:** Axum / state-management
- **Evidence:** `.clone(); // ProviderAdapter is Arc-like: clone is a cheap reference count increment, not a deep copy.`
- **Why it matters:** The comment's mechanism is wrong. `state.provider_adapters` is `Arc<ProviderAdapterRegistry>` (the Arc wraps the registry, not the adapter); `ProviderAdapterRegistry::get()` (`adapter/mod.rs:283`) returns `Option<&ProviderAdapter>`. All four adapter structs are zero-sized unit structs (`OpenAiChatAdapter;`, `AnthropicAdapter;`, `ResponsesAdapter;`, `GeminiAdapter;`), so `.clone()` is a trivial enum-tag copy — no Arc, no refcount increment.
- **Recommendation:** Rewrite to: `// ProviderAdapter variants are zero-sized unit structs; clone is a trivial copy.`
- **Verifier note:** Documentation correctness only; zero runtime/perf impact (the clone is indeed cheap either way).

---

### INFO-2 — `Arc<Counter>` wraps a zero-sized unit struct, so the Arc allocation is pure overhead

- **File:** `crates/llm-proxy-server/src/state.rs:64` (`token_counter: Arc<Counter>`); `crates/llm-proxy-core/src/token/counter.rs:42` (`pub struct Counter;`); constructed once at `state.rs:144`
- **Dimension:** Apollo / pointers-concurrency (arc-over-clone)
- **Evidence:**
  ```rust
  token_counter: Arc<Counter>,          // state.rs:64
  // ... and Counter is:
  #[derive(Debug, Clone, Default)]
  pub struct Counter;                   // crates/llm-proxy-core/src/token/counter.rs:42
  // constructed once:
  token_counter: Arc::new(Counter::new()),   // state.rs:144
  ```
- **Why it matters:** `Counter` is a zero-sized unit type whose `count_tokens`/`count_messages` methods never read `self` (they are pure functions of their `&str` arguments), confirming there is no state to share. Wrapping a ZST in `Arc` allocates an `ArcInner` (atomic counters + a pointer to nothing) purely to share nothing. `Counter` could be a plain field (`token_counter: Counter`) — since `AppState: Clone`, each clone gets a trivial ZST copy at zero cost — or its logic could be an associated function with no state at all. The only hot-path use (`routes/token_count.rs:161`) borrows through the Arc.
- **Recommendation:** Either drop the `Arc` (plain `Counter` field, or make `count_messages` an associated function), or leave as-is if uniformity across `AppState`'s fields is valued. Low impact: the Arc is created once at startup and never cloned on the hot path.
- **Verifier note:** Factual claims accurate. Kept at INFO (not escalated) because (1) every one of `AppState`'s 11 fields is uniformly Arc-wrapped by design, and both the manual `Debug` impl (`state.rs:88-102`) and the `clone_shares_arc_references` test (`state.rs:507`, which asserts `Arc::ptr_eq(&state.token_counter, &cloned.token_counter)`) rely on that uniformity — unwrapping one field would break the test and the pattern; (2) the Arc is created once at startup, so the runtime cost is a single startup allocation.

---

## Findings by Dimension

### Apollo / clippy-linting — 3 findings
- MEDIUM-3 deprecated `is_duplicate` test calls (`middleware.rs:403-485`)
- LOW-2 inert restriction-lint `#[allow]`s (`transport.rs:128`, `anthropic.rs:42,56`, `lib.rs:33`)
- LOW-3 `#[allow(deprecated)]` → `#[expect]` (`anthropic.rs:211,333,506,635,873`)

### Apollo / type-state — 1 finding
- LOW-1 `ProxyRequest.stream` bool / type-state (`transport.rs:62-81`)

### Apollo / performance — 6 findings
- LOW-4 redundant `glob_matches` (`catalog.rs:71-78`)
- LOW-5 per-request headers `HashMap` clone (`provider_registry.rs:392-402`)
- LOW-6 `RateLimiter` double lookup + IP allocation (`middleware.rs:253-259`)
- LOW-7 `format!` key in `record_success` (`metrics.rs:95-98`)
- LOW-8 hex `String` HashMap key (`middleware.rs:123-128`)
- LOW-9 linear catalog scan (`core_pipeline.rs:397-412`)

### Apollo / idioms-ownership — 4 findings
- MEDIUM-4 eager `serde_json::Map` allocation on decode hot path (`openai_chat.rs:167,236`, `anthropic.rs:226`)
- LOW-14 `&Vec<T>` instead of `&[T]` (`adapter/responses.rs:335`)
- LOW-15 `merge_catalog` double id clone (`catalog.rs:51,59`)
- LOW-16 `models` route re-clones ids despite a comment claiming otherwise (`routes/models.rs:126`)

### Apollo / pointers-concurrency — 1 finding
- INFO-2 `Arc<Counter>` over a zero-sized type (`state.rs:64`)

### Apollo / testing — 6 findings
- MEDIUM-6 `cmd_stop`/`cmd_status` untestable — hardcoded global config_dir (`commands/stop.rs:10`, `commands/status.rs:16`)
- LOW-17 launchd plist/desktop entry tested with `contains()` not insta snapshots (`platform.rs:184`)
- LOW-18 SSE stream integration tests assert 7 behaviors in one test (`tests/core_pipeline.rs:791,850,898,946`)
- LOW-19 fixed-sleep readiness + timing-dependent `count < 50` (`transport.rs:436,915,920`)
- LOW-20 `is_toml_config_various` blob + triplicated `validate_toml_extension` tests (`config_validation.rs:62`, `cli_validate.rs`, `cli_serve.rs`)
- LOW-21 zero executable doc-tests; public API documented only with non-rust fenced blocks (`catalog.rs:34,71`, `provider_config.rs:663`)

### Apollo / comments-docs — 5 findings
- LOW-22 broken intra-doc links to `CoreRequest`/`CoreResponse`/`CoreEvent` (`adapter/mod.rs:1`)
- LOW-23 broken intra-doc links to `CHARS_PER_TOKEN`/`PER_MESSAGE_OVERHEAD` (`token/counter.rs:38,75`)
- LOW-24 unresolved module-doc link `[`interpolate_env_vars`]` (`env_interpolate.rs:3`)
- LOW-25 unresolved module-doc link `[`Ordering::Relaxed`]` (`metrics.rs:9`)
- LOW-26 unresolved doc link `[`is_duplicate_with_path`]` (`middleware.rs:80`)

### Axum / middleware-layers — 2 findings
- HIGH-2 zero-timeout DoS (`routes/mod.rs:88-91`)
- MEDIUM-1 request-id/span correlation (`routes/mod.rs:75,87`)

### Axum / production-shutdown — 6 findings
- HIGH-1 unbounded shutdown drain (`serve.rs:97-105`)
- MEDIUM-2 slowloris / no read-header timeout (`serve.rs:88-103`)
- LOW-10 config-side zero-timeout (`state.rs:154-156`)
- LOW-11 missing `CatchPanicLayer` (`Cargo.toml:21`)
- LOW-12 missing security headers (`routes/mod.rs:67-91`)
- LOW-13 health telemetry on unauthenticated route (`health.rs:37-56`)

### Axum / router-routing — 1 finding
- LOW-27 bare axum 405 bypasses protocol-aware fallback (`routes/mod.rs:94-103`)

### Axum / handlers-extractors — 1 finding
- LOW-28 manual `serde_json::from_slice` discards `JsonRejection` taxonomy (`messages.rs:64`, `chat.rs:67`, `token_count.rs:72`)

### Axum / responses-streaming — 1 finding
- MEDIUM-5 client-disconnect events recorded as failures, corrupting error-rate metrics (`core_pipeline.rs:797,804,859`)

### Axum / error-handling-intoresponse — 4 findings
- MEDIUM-7 `TimeoutLayer`/`DefaultBodyLimit` emit non-JSON error bodies (`routes/mod.rs:88-91`)
- LOW-29 rate-limit/dedup run before JSON/provider validation (`messages.rs:55-65`, `chat.rs:58-68`)
- LOW-30 POST handlers accept any Content-Type → misleading 'invalid JSON' 400 (`chat.rs:67-68`)
- LOW-31 inconsistent log level (`info!`/`warn!`/silent) for the same error path (`messages.rs:39`, `models.rs:99`, `chat.rs:39`, `token_count.rs:53`)

### Axum / state-management — 1 finding
- INFO-1 misleading 'Arc-like' comment (`core_pipeline.rs:368`)

---

## Strengths

Grounded observations of what the codebase already does well:

1. **Layered middleware composition is correct in shape.** `build_router` (`routes/mod.rs:67-111`) cleanly separates a `lightweight` router (`/health`, `/ready`, `/version` — `TraceLayer` only) from the provider `api` router (`DefaultBodyLimit` + `TraceLayer` + `TimeoutLayer`), and `routes/mod.rs:60-66` honestly documents exactly what `TimeoutLayer` does and does not cover (handler body, not streaming response body) — a rare and valuable accuracy.
2. **Config validation is real where it matters.** `load_app_config` (`provider_config.rs:949-993`) validates the `hot_reload=true` footgun and enforces strict schema validation for provider routes (per recent commit `58de470`). The gap is that `request_timeout` wasn't included, not that validation is absent.
3. **`RequestDeduplicator` is well-isolated and its deprecation is clean.** The deprecated `is_duplicate` delegates to `is_duplicate_with_path("", body)` (`middleware.rs:82-84`), and the replacement is fully covered by dedicated tests — the defect is only that the redundant old-API tests weren't removed.
4. **Sensitive query parameters are sanitized in debug output** (commit `4ed90f6`), and per-provider/per-model counters are deliberately omitted from the unauthenticated health endpoint (`health.rs:35-36`) — evidence the team thinks deliberately about information disclosure.
5. **Glob/regex compilation is cached** behind `GLOB_CACHE` (`catalog.rs:94-95`), so the redundant-match findings above are mutex-lock + `is_match` cost, not recompilation.
6. **`ProviderAdapter` variants are zero-sized**, making adapter dispatch a no-op copy rather than an allocation — the `Arc<ProviderAdapterRegistry>` design correctly puts the refcount on the registry, not the adapters.
7. **The `token_count.rs` handler demonstrates the correct input-validation ordering** (provider name → existence → rate-limit/dedup → JSON parse) — the LOW-29 finding is precisely that `chat.rs`/`messages.rs` should mirror this established, in-repo pattern, not invent one.
8. **Deterministic readiness helpers already exist in the test tree** (`tests/chat_completions.rs:27-46`, `tests/core_pipeline.rs:32`) — the LOW-19 flaky-sleep finding is an opportunity to propagate an existing workspace pattern, not introduce a new dependency.

---

## Verification Notes

- **Two passes combined.**
  - **First pass:** 43 raw candidates across 6 dimensions (clippy-linting, performance, type-state, middleware-layers, production-shutdown, state-management) → 38 deduped → **19 confirmed**, 5 uncertain, 14 refuted.
  - **Second pass:** 54 raw candidates across 10 additional dimensions (idioms-ownership, testing, comments-docs, pointers-concurrency, router-routing, handlers-extractors, responses-streaming, error-handling-intoresponse, and sub-dimensions thereof) → 52 deduped → **23 confirmed**, 2 uncertain, 27 refuted.
  - **Combined:** 97 raw → **42 confirmed**, 7 uncertain, 41 refuted.
- **Refuted (excluded) — most common reasons** (second pass): the claimed hot path was actually `#[cfg(test)]`; an idiom "violation" had no runtime cost (e.g. `&mut Vec<T>` for a `push` callee, which the candidate wanted to change to a slice that would not compile); a `cargo doc` warning was claimed but did not reproduce; or the finding duplicated an item already in `docs/audit-report.md` (643 findings) or the first pass.
- **Severity adjustments applied (second pass):** 6 findings were lowered from the candidates' proposed severity (4 MEDIUM→LOW on test-quality / error-message-UX / status-code-precision grounds; 2 MEDIUM→LOW where the cited SKILL.md rule did not actually prescribe the claimed behavior). Two more were held at MEDIUM against a proposed HIGH. The four confirmed new MEDIUMs each carry a genuine, non-duplicate consequence: a per-request heap allocation (MEDIUM-4), SLO/telemetry corruption (MEDIUM-5), uncovered command branching (MEDIUM-6), and a client-facing schema break (MEDIUM-7).
- **False-positive rate (second pass):** ~52% (27 refuted / 52 deduped) — higher than the first pass (~37%), reflecting that the second pass probed softer idiom/testing/documentation dimensions where "rule violation" and "defensible style choice" are harder to separate. The severity adjustments above are the mechanism that kept the confirmed set honest.
- **Line-cite caveats:** HIGH-2's tower-http reference (candidate: `service.rs:113-145`) is slightly off — load-bearing lines are `112-119` and `140-149`; substance verified against source. LOW-19's readiness-helper cite (`src/routes/chat_completions.rs:37`) is a module-path error — the helper lives in `tests/chat_completions.rs:27-46` and `tests/core_pipeline.rs:32`; the recommendation still holds. LOW-23's "3 warnings" count is correct for this file but the total `cargo doc` output includes 2 unrelated warnings. All other file:line cites were verified against the working tree at audit time.

---

## Coverage Gaps (Third Pass)

**Goal of this pass:** the first two passes found 42 findings but did not ask the inverse question — *what did they not look at?* This pass deliberately audited the surface the first two passes had structurally excluded, using a dynamic multi-agent finder/verifier workflow (28 raw candidates → 15 confirmed + 7 refuted; one finder dimension died on a 429 and was re-run in isolation, yielding 3 more). Every MEDIUM below was **independently reproduced against source by the orchestrator**, not trusted to the workflow output alone. 4 MEDIUM and 15 LOW findings are added; one candidate (`refresh_locks`/`refresh_outcomes` unbounded growth) was correctly suppressed as a duplicate of `docs/audit-report.md` #234.

### Headline: two Apollo skill chapters were entirely absent from passes 1–2

- **Ch4 (Error Handling) — `thiserror`/`anyhow` discipline, `Result` vs panic, error-type design.** The audit's "Axum / error-handling-intoresponse" dimension covers how errors *render to HTTP* (IntoResponse, JSON schema) — it never examined the *library* error-type discipline: `anyhow` leaking into a public library API, `Box<dyn Error>` sources that erase typed context, `.expect()`/`.unwrap()` on genuinely-fallible production paths, and error types missing `PartialEq` for testability. This pass surfaces 5 such findings (GAP-LOW-10 through GAP-LOW-15, two of them safety-relevant: `.expect()` at daemon startup and `anyhow` in a `pub` library API).
- **Ch6 (Generics & Dispatch) — static vs dynamic dispatch, `Box<dyn>` vs enum.** No dimension examined the provider-adapter / stream-decoder dispatch sites. The one finding here (GAP-LOW-1) is low-severity, but the chapter had zero coverage.
- **Secret-redaction & untrusted-input robustness — a dimension that did not exist.** The audit treated redaction as a *strength* (Strengths §4: "Sensitive query parameters are sanitized in debug output"). That is true for the case it examined, but no dimension pressure-tested the redaction *completeness* (does the regex cover real token shapes?), the *egress paths* (does every error string toward a client pass through the key-pattern sanitizer?), or *untrusted-upstream DoS* (can a malicious/buggy provider OOM or stall the proxy?). This pass found that surface to be the richest source of new MEDIUMs: **3 of the 4 new MEDIUMs (GAP-MED-1, GAP-MED-2, GAP-MED-3) are security/availability**, more than the rest of the audit combined.
- **The `apps/llm-proxy` binary crate beyond `serve.rs`.** Passes 1–2 audited `serve.rs` (production-shutdown) but not `autostart`, `models`, `init`, `pid`, or the daemon-spawn path. This pass found GAP-MED-4 (autostart clobber) and GAP-LOW-10 there.

### Newly surfaced MEDIUM findings

#### GAP-MED-1 — Secret-redaction regex stops at `.`: dotted keys / JWTs are only partially redacted and the suffix leaks to the client

- **File:** `crates/llm-proxy-provider/src/error.rs:100-124` (redaction patterns); egress at `crates/llm-proxy-server/src/routes/error_response.rs:296`
- **Dimension:** Security / secret-redaction (Ch4 §4.4, Ch8 — the module's own doc at `error.rs:148-161` enumerates covered formats and is now inaccurate)
- **Evidence:** every key/`Bearer` pattern shares the character class `[A-Za-z0-9_-]`, which excludes the literal `.` (and `/`, `+`, `=`). The sanitizer runs at construction inside `ProviderError::api()` (`error.rs:161` `sanitize_api_error_body`); that (partially-redacted) body is later rendered to the client at `error_response.rs:296` `RouteError::Upstream { .. } => truncate_error_body(&sanitize_upstream_body(&body))`.
- **Why it matters:** when an upstream 4xx/5xx echoes the supplied credential in a dotted format — an OpenAI restricted key `sk-proj-AbCd…​.T3BlbkFJ…`, a `Bearer <JWT>` (`eyJ…​.eyJ…​.<sig>`), or a legacy dotted key — the regex matches only up to the first dot and the secret-bearing suffix survives into the client response. **Reproduced against the exact 12 patterns:**
  - `invalid key: sk-proj-AbCdEfGh1234567890.T3BlbkFJabc123…` → `invalid key: ***.T3BlbkFJabc123…` (suffix leaks)
  - `Bearer eyJhbGci…​.<body>.<sig>` → `***.<body>.<sig>` (payload + signature leak)
  - controls `AIza…` and `sk-ant-…` (dotless) redact fully → `***` ✓ — confirming the gap is specifically the `.` boundary.
- **Recommendation:** broaden the character class in the `Bearer`, `sk-`, `sk-ant-`, `AIza`, `key-` and generic patterns to the URL/JSON-safe punctuation real tokens carry: `[A-Za-z0-9_.\-/+=]{N,}`, anchored so a trailing quote/brace isn't consumed. Add an explicit `sk-proj-…` (dotted) pattern and a JWT-shaped `Bearer eyJ…` pattern. Add a unit test asserting a dotted key round-trips to `***` with no suffix.
- **Verifier note:** held at MEDIUM, not HIGH — the leak is *conditional* on an upstream echoing the credential in its error body. Major providers (OpenAI, Anthropic) return generic `invalid_api_key` without echoing; the realistic exposure is custom/self-hosted gateways or JWT-issuing endpoints. But the consequence is a *client-facing* secret leak (worse than a log-only leak), redaction is the codebase's stated defense, and the fix is localized — so MEDIUM.

---

#### GAP-MED-2 — SSE parser buffer grows without bound; a misbehaving upstream can OOM the proxy (no `max_buffer_bytes`, no stream timeout)

- **File:** `crates/llm-proxy-provider/src/sse.rs:64-67` (`push_chunk`/`extend_from_slice`), `:123-145` (`drain_buffer`), `:48` & `:182` (`current_data_lines` unbounded); `crates/llm-proxy-provider/src/transport.rs:139-146` (no `.timeout()`); consumer `crates/llm-proxy-server/src/routes/core_pipeline.rs:871`
- **Dimension:** Availability / untrusted-upstream robustness (Ch4 robustness)
- **Evidence:** `push_chunk` does `self.buffer.extend_from_slice(chunk)` with no size check; `drain_buffer` only drains inside `while let Some(nl_pos) = self.buffer.iter().position(|&b| b == b'\n')` — if an upstream chunk has no `\n`, the loop body never runs and `Ok(empty)` is returned, so every byte is retained. `process_line` pushes into `current_data_lines: Vec<String>` (`sse.rs:48`) with no cap, so a valid-but-huge multi-line `data:` frame also grows unbounded. The streaming transport that feeds these bytes (`ProxyClient`, `transport.rs:139-146`) sets only `connect_timeout(10s)`; there is **no `.timeout()`** on the request, so reqwest holds the body stream open indefinitely.
- **Why it matters:** a compromised or buggy upstream that dribbles non-newline bytes, or emits a valid but multi-gigabyte `data:` frame, grows the per-connection buffer without bound → OOM the proxy process. Verified against source: no `max_buffer_bytes`/`max_line_bytes` anywhere; `drain_buffer` confirmed newline-gated.
- **Recommendation:** add a configurable `max_buffer_bytes` to `SseFramer`; in `push_chunk`, after `extend_from_slice`, return `Err(ProviderError::SseFraming(...))` if exceeded — `core_pipeline.rs:871` already closes the stream with an in-band error on `Err`, so this is a clean failure, not a panic. Separately give the streaming `ProxyClient` an overall response timeout (or per-chunk idle read timeout) so a stalled upstream can't hold the connection forever.
- **Verifier note:** MEDIUM, not HIGH — the attacker path requires the upstream provider (already trusted with API keys) to be compromised or buggy; it is not an unauthenticated-public-internet attack. But it is a real, reproducible OOM with a localized fix.

---

#### GAP-MED-3 — Daemon log file is create-then-chmod (TOCTOU), reintroducing the exact window `create_private_file` exists to close

- **File:** `apps/llm-proxy/src/commands/serve.rs:141-148`; contrast `apps/llm-proxy/src/permissions.rs:48-62` (`create_private_file`)
- **Dimension:** Security / file-permission hygiene (Ch9, Ch4 §4.4)
- **Evidence:** `spawn_daemon` opens the log with `File::options().create(true).append(true).open(&log_path)` (`serve.rs:142-146`) — file born under the process umask (typically 0644, world-readable) — **then** calls `set_private_permissions(&log_path)` (`serve.rs:148`) to chmod it to 0600. The project's own `create_private_file` (`permissions.rs:51-62`) exists specifically to avoid this window: its doc comment states it "uses `OpenOptions` with `mode(0o600)` to set permissions **atomically at creation time, avoiding a window where the file exists with default umask permissions**." `serve.rs` reimplements the create-then-chmod pattern that helper was written to eliminate. Immediately after, `serve.rs:149-154` redirects the daemon's stdout **and** stderr (hence tracing output) into this handle.
- **Why it matters:** the briefly-world-readable file holds daemon logs/stderr — and GAP-MED-1 just proved redaction can leak key suffixes, so a leaked segment landing in this log is briefly readable by any local user who opens the fd in that window (an open fd retains read access even after the later chmod). The inconsistency with `create_private_file` is the clearest signal this is a regression, not an intentional trade-off.
- **Recommendation:** on Unix use `OpenOptions::new().create(true).append(true).mode(0o600).open(&log_path)` (`OpenOptionsExt`) so the file is born private, mirroring `create_private_file`; fall back to create-then-chmod on non-Unix.
- **Verifier note:** MEDIUM — the open→chmod window is narrow and requires local-user access + timing, but the file holds process logs that may reference requests/config, the fix is trivial and already implemented once elsewhere in the codebase, and the project clearly *intends* atomic private-file creation.

---

#### GAP-MED-4 — `autostart enable` silently clobbers a hand-edited launchd plist / .desktop entry

- **File:** `apps/llm-proxy/src/commands/autostart.rs:53-54` (plist), `:69` (.desktop); contrast `apps/llm-proxy/src/commands/init.rs` (overwrite-guarded)
- **Dimension:** CLI / data-loss (Ch4 — predictable behavior / Result over silent clobber)
- **Evidence:** `cmd_autostart_enable` writes the plist with `std::fs::write(&plist_path, plist_content)` (`autostart.rs:53`) and the `.desktop` entry the same way (`:69`), with no existence check. This diverges from the project's own conventions: `init.rs` guards `config.toml` with `if config_path.exists() { bail!("config file already exists …") }`, and `permissions.rs::create_private_file` uses `create_new(true)` with a dedicated "rejects existing" test.
- **Why it matters:** a user who hand-edited the plist (added `KeepAlive`, custom `EnvironmentVariables`, `StartInterval`) loses their edits on the next `autostart enable`, with no warning and no backup. Re-running `autostart enable` with a different `--config`/`--port` also silently rewrites the launchd unit to point elsewhere.
- **Recommendation:** check `plist_path.exists()` / `desktop_path.exists()` first and bail with a message offering `--force` (or prompt), mirroring `init.rs`'s overwrite protection.
- **Verifier note:** MEDIUM but at the LOW/MED boundary — `autostart enable` is an infrequent, explicitly-invoked setup command (cool path), so the data-loss risk is real but narrow. The clear, fixable asymmetry with `init.rs` keeps it above LOW.

---

### Newly surfaced LOW findings

| ID | Title | File:line | Skill |
|---|---|---|---|
| GAP-LOW-1 | `Box<dyn ProviderStreamDecoder + Send>` allocates per stream + vtable-dispatches per frame despite a closed 4-impl set (sibling `ProviderAdapter` enum already static-dispatches the same set) | `crates/llm-proxy-provider/src/adapter/mod.rs:207-217`; box stored `crates/llm-proxy-server/src/routes/core_pipeline.rs:767`, decode at `:901,:988` | Ch6 §6.5–6.6 |
| GAP-LOW-2 | Divergent sanitizers: the streaming in-band error path and the non-`Api` error path run only `sanitize_upstream_error_body` (URL-redact + truncate), never re-applying key patterns — a future error string carrying a key would not be key-redacted | `crates/llm-proxy-server/src/routes/core_pipeline.rs:955, 1218, 1223`; sanitizer `:1248-1259` | Ch4 (defense-in-depth) |
| GAP-LOW-3 | `TraceLayer::new_for_http()` (no `make_span_with`) logs the full request **URI including query string** in every span; several LLM SDKs fall back to `?api_key=`/`?key=` query params | `crates/llm-proxy-server/src/routes/mod.rs:75, 87` | Ch4 (log hygiene) |
| GAP-LOW-4 | Provider discovery has no retry/backoff — one transient 5xx/TLS-reset mid-pagination aborts the whole catalog refresh for a provider | `crates/llm-proxy-provider/src/discovery.rs:68-84`; caller `crates/llm-proxy-server/src/catalog_service.rs:121-127` | Ch4 (resilience) |
| GAP-LOW-5 | `set_query_parameter` collects all query pairs into `Vec<(String,String)>` with double `into_owned()` per pair then rebuilds the query (needless_collect) | `crates/llm-proxy-provider/src/discovery.rs:213-225` | Ch1/Ch3 |
| GAP-LOW-6 | Discovery silently drops malformed model records (`warn!`/`debug!` + `continue`) with no aggregate dropped-count surfaced in the success path | `crates/llm-proxy-provider/src/discovery.rs:169-191` | Ch4/observability |
| GAP-LOW-7 | `make_chunk` clones `id` and `model` (owned `String`) on every delta event in the streaming hot path (6+ call sites in `encode_event`) | `crates/llm-proxy-protocol/src/client/openai_chat.rs:876-885`; origins `:577-579` | Ch3/Ch1 |
| GAP-LOW-8 | OpenAI Chat `decode_request` stop-array handling silently ignores non-string items, while the canonical `core::deserialize_stop` returns `Err` for the same input — the two wire-format rules have diverged | `crates/llm-proxy-protocol/src/client/openai_chat.rs:246-258` vs `crates/llm-proxy-protocol/src/core.rs:589-606` | Ch4/Ch1 §1.6 |
| GAP-LOW-9 | `StopReason::Unknown` discards the original provider stop value during normalization (documented data loss) with no linked tracking issue; sibling `CacheControlType::Other(String)` already solves this in the same file | `crates/llm-proxy-protocol/src/core.rs:798-804` | Ch8 |
| GAP-LOW-10 | `.expect("Rfc3339 formatting is infallible…")` on the live `--models --write-catalog` path turns a future time-crate quirk into a panic that aborts a user command after a successful network fetch | `apps/llm-proxy/src/commands/models.rs:104-106`; sibling fallback `crates/llm-proxy-server/src/catalog_service.rs:253-257` | Ch4 |
| GAP-LOW-11 | `CoreError::ConfigValidation` / `ProviderResolution` store their source as `Option<Box<dyn std::error::Error + Send + Sync>>`; `From<ConfigValidationError>` boxes the well-typed enum, so downstream callers cannot downcast to the specific variant | `crates/llm-proxy-core/src/error.rs:27-29, 36-38, 81-87` | Ch4 §4.4 / Ch9 |
| GAP-LOW-12 | `ConfigValidationError` (20+ structured variants, `String` fields only) lacks `PartialEq`/`Eq`, forcing tests into brittle `to_string().contains(...)` substring assertions (e.g. `error.rs:136`) | `crates/llm-proxy-core/src/provider_config.rs:458-460` | Ch4 §4.6 |
| GAP-LOW-13 | `ProxyClient::new()` `.expect()`s on a genuinely fallible `reqwest::Client::builder().build()` (fails on invalid `HTTP_PROXY`/`HTTPS_PROXY` env or TLS-init failure) at daemon startup; the `#[allow(clippy::expect_used)]` + self-acknowledging doc are the tell | `crates/llm-proxy-provider/src/transport.rs:128-131`; startup call `apps/llm-proxy/src/commands/serve.rs:46` | Ch4 §4.5 |
| GAP-LOW-14 | `anyhow::Result` in the **public API** of `PidManager`, a `pub` type in a library crate (`llm-proxy-core`) — erases typed error context a caller may need | `crates/llm-proxy-core/src/pid.rs:10, 14` | Ch4 §4.4 |
| GAP-LOW-15 | `shutdown_signal()` uses `.expect()` on `signal::ctrl_c()` install, and the SIGTERM-install-failure branch falls back to `std::future::pending()` (never resolves) — if SIGTERM-handler install fails and the user doesn't Ctrl-C, graceful shutdown never triggers | `crates/llm-proxy-server/src/shutdown.rs:8, 19` | Ch4 |

### Correctly-suppressed duplicate (not counted above)

- **Unbounded insert-only growth of `refresh_locks`/`refresh_outcomes`** (`crates/llm-proxy-server/src/catalog_service.rs:25-26, 94-98, 162-179`) — verified accurate (no `.remove()`/`.clear()`/`.retain()` anywhere) but identical to **`docs/audit-report.md` #234**. Listed here only so the dimension ("config hot-reload internals") is recorded as *examined*, not *missed*.

### Coverage notes — intentional non-findings (so they are not mistaken for gaps)

- **`llm-proxy-storage` and `llm-proxy-api` crates appear "unaudited" but are empty-by-design.** Each is a v1 placeholder (`pub mod placeholder { pub struct NotImplemented; }`, see `crates/llm-proxy-storage/src/lib.rs:15-23` and `crates/llm-proxy-api/src/lib.rs:14-22`) reserving a namespace until the protocol-normalization/protocol-mini designs land. No real code to audit; not a coverage gap.
- **`anyhow` in `apps/llm-proxy/`** (the binary crate) is permitted by Ch4 §4.4 and is used correctly with `.with_context()` throughout the commands. Not a finding.

### Verification notes (third pass)

- **Method:** an 8-dimension dynamic finder/verifier workflow (find → adversarial verify per candidate, matching passes 1–2's verifier pattern). 28 raw candidates → **15 confirmed, 7 refuted**. The `ch4-errors` finder died on a 429 rate-limit at the finder stage (returning zero findings despite the dimension being real), so it was **re-run in isolation**, yielding 3 more confirmed (GAP-LOW-11/12/13); GAP-LOW-14 and GAP-LOW-15 were independently confirmed by the orchestrator from source.
- **Independent reproduction (not trusted to the workflow):** all 4 MEDIUMs were re-verified by the orchestrator against source after the workflow returned — GAP-MED-1 by re-running the exact 12 redaction patterns in Python against dotted/JWT/dotless-control inputs; GAP-MED-2 and GAP-MED-3 by reading `sse.rs`/`transport.rs`/`serve.rs`/`permissions.rs` directly; GAP-MED-4 by reading `autostart.rs` and confirming the asymmetry with `init.rs`. GAP-MED-1's egress was additionally confirmed at `error_response.rs:296`.
- **Severity discipline:** one workflow candidate (`CoreError` `Box<dyn Error>` source, GAP-LOW-11) was **downgraded MEDIUM→LOW** because no current consumer downcasts, so the consequence is latent API hygiene rather than a live defect — applying the same bar passes 1–2 used (their MEDIUMs "each carry a genuine, non-duplicate consequence"). GAP-MED-4 is flagged as sitting at the LOW/MED boundary.
- **Line-cite corrections:** GAP-LOW-1's finder initially cited the decoder box under `provider/`; the box is actually stored in `crates/llm-proxy-server/src/routes/core_pipeline.rs:767`. GAP-LOW-10's finder cited a nonexistent `apps/llm-proxy/src/services/catalog_service.rs`; the actual sibling fallback is `crates/llm-proxy-server/src/catalog_service.rs:253-257`. All cites above reflect the corrected paths.

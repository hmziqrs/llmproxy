# Comprehensive Audit Report

> **Date:** 2026-06-12 | **Scope:** Full workspace (7 crates, ~39K lines)
> **Methodology:** Dynamic workflow with parallel deep-read auditors + adversarial verification per finding

## Executive Summary

| Metric | Value |
|--------|-------|
| Crates audited | 7 |
| Lines of Rust | ~39,126 |
| Total findings (deduplicated) | 643 |
| Rejected by adversarial verification | ~200 (false positives filtered) |
| **HIGH** | **8** |
| **MEDIUM** | **113** |
| **LOW** | 355 |
| **INFO** | 167 |

## Per-Crate Breakdown

| Crate | Lines | Findings | HIGH | MED | LOW | INFO |
|-------|-------|----------|------|-----|-----|------|
| `llm-proxy-core` | 3,177 | 147 | 1 | 13 | 74 | 59 |
| `llm-proxy-protocol` | 10,145 | 101 | 1 | 16 | 67 | 17 |
| `llm-proxy-provider` | 12,513 | 121 | 0 | 14 | 45 | 62 |
| `llm-proxy-server` | 8,572 | 193 | 3 | 48 | 123 | 19 |
| `llm-proxy (bin) + stubs` | 1,678 | 82 | 3 | 22 | 47 | 10 |

## By Category

- **correctness**: 119
- **test-coverage**: 118
- **maintainability**: 115
- **api-design**: 80
- **performance**: 66
- **security**: 65
- **error-handling**: 41
- **idiomatic-rust**: 39

---

## 🔴 HIGH Findings

### H1. PID file TOCTOU race: check-then-write without atomic creation

- **File:** `apps/llm-proxy/src/main.rs:483`
- **Category:** security

**Problem:**

cmd_serve reads the PID file (line 483), checks if the process is running (line 484), then writes a new PID file (line 496). Between read_pid() and write_pid(), another concurrent `llm-proxy serve` can pass the same check and both processes start, each overwriting the other's PID. The PID file is created with File::create (line 342) which is not exclusive -- it truncates an existing file. This should use OpenOptions::new().write(true).create_new(true) for atomic exclusive creation, or use flock(2) / fcntl(F_SETLK) for proper mutual exclusion.

**Recommendation:**

Use OpenOptions::new().write(true).create_new(true) to atomically create the PID file, or use file locking (e.g., `flock` via the `fs4` crate) to guard the entire check-and-create sequence.

### H2. TOCTOU race in PID file write between stale-check and write_pid

- **File:** `apps/llm-proxy/src/main.rs:496`
- **Category:** security

**Problem:**

The sequence at lines 483-496 is: (1) read PID file, (2) check if process is running, (3) clean up stale file, (4) write new PID file. Between steps 2-3 and 4, a concurrent `llm-proxy serve` process could pass the same check and also write a PID file, resulting in two server instances believing they hold the PID file. The PID file is not created with O_EXCL or atomically.

**Recommendation:**

Use `std::fs::OpenOptions::new().write(true).create_new(true)` (O_EXCL) for the PID file creation so that a second process fails atomically. Alternatively, use file locking (e.g., `flock` or `fcntl` advisory lock) on the PID file.

### H3. cmd_stop is a no-op on non-Unix platforms

- **File:** `apps/llm-proxy/src/main.rs:603`
- **Category:** correctness

**Problem:**

The SIGTERM send (line 612-619) and SIGKILL send (line 634-640) are both gated behind `#[cfg(unix)]`. On non-Unix platforms (Windows), cmd_stop() will print 'sent SIGTERM to PID {pid}' (line 621) even though no signal was actually sent. It will then enter the polling loop, which will see the process still running, and print 'server force-stopped (PID {pid})' (line 642) without having actually stopped anything. is_process_running() also always returns `true` on non-Unix (line 387-390), making the entire stop command a deceptive no-op.

**Recommendation:**

Add a `#[cfg(not(unix))]` block in cmd_stop() that prints an error like 'stop is not supported on this platform' and returns early, or implement Windows process termination using `OpenProcess`/`TerminateProcess` via the `windows-sys` crate.

### H4. is_process_running returns false when process exists but caller lacks permission (EPERM not handled)

- **File:** `crates/llm-proxy-core/src/pid.rs:93`
- **Category:** correctness

**Problem:**

The Unix implementation of `is_process_running` returns `false` when `libc::kill(pid, 0)` returns -1 with `errno == EPERM`. EPERM means the process DOES exist but the current user lacks permission to signal it. Returning `false` in this case causes the caller to incorrectly conclude the process is dead, potentially leading to stale-PID cleanup that removes the PID file of a live daemon. The main.rs binary has already fixed this bug (lines 366-384 of apps/llm-proxy/src/main.rs) by checking for EPERM and returning `true`, but the library version was never updated.

**Recommendation:**

Match the main.rs implementation: check errno after a non-zero return from kill(). If errno is EPERM, return true (process exists but we lack permission). Only return false on ESRCH (no such process). Use `std::io::Error::last_os_error()` or platform-specific errno access to avoid raw `unsafe` errno reads. Example fix:

```rust
#[cfg(unix)]
{
    let ret = unsafe { libc::kill(pid as i32, 0) };
    if ret == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}
```

### H5. ChatMessage.content is String but OpenAI API allows structured content arrays

- **File:** `crates/llm-proxy-protocol/src/openai.rs:111`
- **Category:** correctness

**Problem:**

ChatMessage.content is declared as `pub content: String` (line 121) with `#[serde(default)]`. However, the OpenAI Chat Completions API allows `content` to be either a string or an array of content parts (e.g., `[{"type": "text", "text": "..."}, {"type": "image_url", "image_url": {...}}]`). Deserializing a user message with image_url content parts will fail with a serde type mismatch, causing 400 errors for multi-modal requests sent by clients using the content-array format.

**Recommendation:**

Change `content` to a polymorphic type (e.g., an enum with String and Vec variants) with a custom deserializer that handles both forms. At minimum, add `#[serde(alias = "content")]` or use `serde_json::Value` with a helper method. This is a known limitation that should be documented if not addressed.

### H6. No test for load_disk_cache validation edge cases

- **File:** `crates/llm-proxy-server/src/catalog_service.rs:317`
- **Category:** test-coverage

**Problem:**

There are no tests for load_disk_cache's symlink rejection, file type validation, or provider name mismatch detection. These are important security and correctness paths.

**Recommendation:**

Add tests that verify: (1) symlinked cache files are rejected, (2) non-regular files are rejected, (3) provider name mismatch in the cache file returns an error, (4) malformed TOML in the cache file returns an error.

### H7. Unbounded HashMap growth in RequestDeduplicator is a DoS vector

- **File:** `crates/llm-proxy-server/src/middleware.rs:87`
- **Category:** security

**Problem:**

Every unique `(path, body)` pair inserts a new SHA-256 key into `in_flight`. An attacker can send many requests with unique bodies (trivially done by varying a nonce field in the JSON) to grow the HashMap without bound until OOM. Pruning only removes entries older than `window_ms`, so at any sustained request rate the map grows linearly with unique payloads. There is no cap on the number of in-flight entries.

**Recommendation:**

Cap the HashMap size (e.g., `if map.len() >= MAX_ENTRIES { return false; /* or reject */ }`). Consider an LRU eviction policy or reject new requests when the map is full.

### H8. RateLimiter HashMap keyed by arbitrary client IP string is spoofable and unbounded

- **File:** `crates/llm-proxy-server/src/middleware.rs:200`
- **Category:** security

**Problem:**

Two issues compound here. (1) The `client_ip` string is caller-provided and, when `trust_forwarded_headers` is true, comes directly from the `X-Forwarded-For` header (see `get_client_ip` at line 283). An attacker can forge arbitrary values, creating unlimited independent buckets and bypassing the rate limit entirely. (2) Even without spoofing, there is no cap on the number of buckets. A botnet or IP-range scan creates unbounded entries.

**Recommendation:**

(1) Validate that the extracted IP is a syntactically valid `IpAddr` before using it as a bucket key. (2) Cap the total number of buckets; evict the least-recently-used when full. (3) Consider using the raw `SocketAddr` IP as the key rather than a string.

---

## 🟠 MEDIUM Findings

### Correctness

| # | Title | File:Line | Description |
|---|-------|-----------|-------------|
| M1 | resolve_serve_config and resolve_config are duplicate functions | `apps/llm-proxy/src/main.rs:248` | Lines 248-260 (resolve_serve_config) and lines 268-279 (resolve_config) are functionally identical. Both check CLI path, then LLM_PROXY_CONFIG env var, then fall back to default_config_path(). The ... |
| M2 | Daemon spawn has a race: PID file is written by child but parent reports child PID before PID file exists | `apps/llm-proxy/src/main.rs:467` | In spawn_daemon (line 544), the parent spawns the child, reads child.id() (line 586), prints it (line 587), and detaches. But the PID file is written by the child process later in cmd_serve at line... |
| M3 | PID file TOCTOU race: read_pid + write_pid is not atomic | `apps/llm-proxy/src/main.rs:496` | In cmd_serve, the sequence at lines 483-496 is: (1) read PID file, (2) check if process is running, (3) remove stale PID file, (4) write new PID file. Between steps 1 and 4, another process could s... |
| M4 | cmd_stop is a no-op on non-Unix platforms | `apps/llm-proxy/src/main.rs:612` | On non-Unix targets, the `cmd_stop` function (lines 603-650) reads the PID file, checks if the process is running (always returns true on non-Unix per line 389), prints 'sent SIGTERM to PID', sleep... |
| M5 | cmd_stop sends SIGKILL after only 2 seconds, no SIGKILL on non-Unix platforms | `apps/llm-proxy/src/main.rs:615` | The stop command waits only 10 iterations of 200ms (2 seconds total, line 624-631) before escalating to SIGKILL. For a proxy handling long-running streaming requests, 2 seconds may not be enough fo... |
| M6 | Test acquires TestEnvLock but does manual env var cleanup instead of using EnvVarGuard | `crates/llm-proxy-core/src/env_interpolate.rs:78` | The interpolates_known_var test (env_interpolate.rs:77-88) acquires TestEnvLock but then calls unsafe { std::env::set_var(...) } and unsafe { std::env::remove_var(...) } manually. If the test panic... |
| M7 | TOCTOU race between read_pid existence check and file read | `crates/llm-proxy-core/src/pid.rs:46` | In `read_pid()`, the method first checks `self.pid_file.exists()` and then reads the file in a separate call. Between these two operations, the file could be deleted by another process, causing `re... |
| M8 | TOCTOU race between remove_pid existence check and file removal | `crates/llm-proxy-core/src/pid.rs:76` | In `remove_pid()`, the method checks `self.pid_file.exists()` before calling `remove_file`. Between these two operations, another process could remove the file, causing `remove_file` to fail with `... |
| M9 | Token count uses byte length instead of character count for multi-byte text | `crates/llm-proxy-core/src/token/counter.rs:49` | count_tokens uses text.len() which returns byte length, not character count. For ASCII text this is fine, but for multi-byte Unicode content (CJK characters, emoji, accented characters) this overes... |
| M10 | system_text() concatenates text blocks without separator | `crates/llm-proxy-protocol/src/anthropic.rs:88` | When the `system` field is an array of `SystemContentBlock`s, `system_text()` concatenates all `text` fields with no separator (line 83: `text.push_str(&t)`). This means `[{"type":"text","text":"Yo... |
| M11 | content_blocks() silently drops unparseable array items without any logging | `crates/llm-proxy-protocol/src/anthropic.rs:228` | When `Message::content_blocks()` encounters an array element that fails `serde_json::from_value::<ContentBlock>()`, the error branch silently discards the item. There is no `tracing::warn!` or any ... |
| M12 | decode_system silently drops blocks that fail deserialization | `crates/llm-proxy-protocol/src/client/anthropic.rs:136` | In decode_system(), when iterating over system array items, `serde_json::from_value::<SystemContentBlock>(item.clone())` failures are silently ignored -- the `if let Ok(block)` simply skips the ite... |
| M13 | Silent data loss when tool_use block has missing id/name in Anthropic decode | `crates/llm-proxy-protocol/src/client/anthropic.rs:193` | In decode_content_block, when block type is "tool_use", the id and name fields default to empty strings via unwrap_or_default() if they are absent. An empty-string tool_use_id is functionally meani... |
| M14 | tool_result decode synthesizes empty text block when all inner blocks fail to parse | `crates/llm-proxy-protocol/src/client/anthropic.rs:234` | At line 234, when a tool_result has array content but ALL inner blocks fail `serde_json::from_value` deserialization, the code falls through to the `if blocks.is_empty()` check and synthesizes an e... |
| M15 | Silent data loss when tool_result has missing tool_use_id in Anthropic decode | `crates/llm-proxy-protocol/src/client/anthropic.rs:264` | When decoding a tool_result block, tool_use_id uses unwrap_or_default(), producing an empty string if the field is absent. An empty tool_use_id makes it impossible to correlate the result with its ... |
| M16 | deserialize_stop accepts empty array without rejection | `crates/llm-proxy-protocol/src/core.rs:502` | The custom deserializer for `SamplingOptions::stop` accepts `[]` and produces `Some(vec![])`. An empty stop sequence list has no semantic meaning -- it asks the model to stop on zero sequences, whi... |
| M17 | ToolResult content only extracts Text blocks, silently dropping Images and other types | `crates/llm-proxy-provider/src/adapter/anthropic.rs:921` | In `encode_content_blocks` (lines 907-920), the `CoreContent::ToolResult` branch only extracts `CoreContent::Text` variants from the inner `result_content` vec, concatenating them into a single str... |
| M18 | ToolResult is_error field is silently ignored during encode | `crates/llm-proxy-provider/src/adapter/gemini.rs:297` | When encoding a `CoreContent::ToolResult` (lines 291-335), the `is_error` field is never consulted. Gemini's API does not have a native error flag on functionResponse, but error results should idea... |
| M19 | tool_calls without function name produce no ToolCallStart but arguments may still be emitted | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:170` | When a tool call delta arrives with an empty function name (line 179: `func_name.is_empty()` leads to `continue`), no `ToolCallStart` event is emitted and no entry is added to `self.tool_blocks`. H... |
| M20 | Mixed text and ToolUse content within a single message produces duplicate role entries | `crates/llm-proxy-provider/src/adapter/responses.rs:397` | Lines 408-499 build input items per message. When a message contains both Text and ToolUse variants, ToolUse items are pushed immediately with the role (line 439-447), while text is collected in te... |
| M21 | HTTP error status checked after consuming entire response body | `crates/llm-proxy-provider/src/discovery.rs:134` | In `fetch_page`, the response body is fully buffered into `bytes` (lines 124-133), and only after the entire stream is consumed does the code check `status.is_success()` (line 134). If the status i... |
| M22 | finish() does not handle trailing content that looks like partial UTF-8 at buffer tail | `crates/llm-proxy-provider/src/sse.rs:73` | In `drain_buffer`, the design correctly leaves trailing bytes (potential partial UTF-8) in the buffer after the last newline. When `finish()` is called later, it decodes the entire remaining buffer... |
| M23 | extract_content_variant does not match RedactedThinking variant | `crates/llm-proxy-provider/tests/fixture_tests.rs:443` | The `extract_content_variant` function's matches! macro lists Text, ToolUse, ToolResult, Thinking, Image, Document, Audio, Video, and Refusal -- but omits RedactedThinking. Since CoreContent has a ... |
| M24 | map_catalog_error does not cover all ProviderError variants exhaustively | `crates/llm-proxy-server/src/routes/models.rs:161` | The match in `map_catalog_error` (lines 162-179) handles `Api`, `Http`, and a catch-all `_ =>` arm. However, the catch-all maps to `RouteError::Internal(error.to_string())` which will send a generi... |
| M25 | Token count is a heuristic approximation that excludes significant content | `crates/llm-proxy-server/src/routes/token_count.rs:143` | Lines 76-86 and the implementation explicitly skip tool definitions, non-text content (images, documents), tool_use blocks, and tool_result blocks. For requests that heavily use tools or multimodal... |
| M26 | Race condition: 50ms sleep to wait for mock server readiness | `crates/llm-proxy-server/tests/chat_completions.rs:190` | All spawn_mock_server variants (lines 175-195, 208-239, 242-263, 266-299, 821-826, 1405-1408, 1460-1463) bind to port 0, spawn axum::serve in a tokio task, then sleep 50ms to wait for the server to... |
| M27 | Race condition: 50ms sleep for mock server readiness is fragile | `crates/llm-proxy-server/tests/core_pipeline.rs:245` | Line 245 (also 308, 339, 375, 400, 420, 452, 485, 509, 544): `tokio::time::sleep(Duration::from_millis(50)).await` is used to wait for the mock server to start accepting connections. Under CI load ... |

### Security

| # | Title | File:Line | Description |
|---|-------|-----------|-------------|
| M1 | PID file written without exclusive lock -- subject to daemonization race | `apps/llm-proxy/src/main.rs:336` | write_pid() uses File::create (O_CREAT \| O_TRUNC \| O_WRONLY) without O_EXCL. When two `llm-proxy serve --background` invocations race, both pass the stale-PID check at line 483, both write PID fi... |
| M2 | PID file written without restrictive permissions (world-readable by default) | `apps/llm-proxy/src/main.rs:336` | write_pid() (line 336) creates the PID file using std::fs::File::create() which uses the process umask. Unlike cmd_init() which calls set_private_permissions() after file creation, write_pid() neve... |
| M3 | is_process_running does not validate PID is non-zero | `apps/llm-proxy/src/main.rs:366` | The SAFETY comment on line 368-369 states 'The pid is validated to be a non-zero positive integer by the caller (read from PID file)', but read_pid() (line 321) parses a u32 which can be 0. If a co... |
| M4 | Daemon log file created with default permissions and appends to existing file | `apps/llm-proxy/src/main.rs:562` | At line 563, the daemon log file is opened with `.create(true).append(true)` at a predictable path (`~/.config/llm-proxy/llm-proxy.log`). The file is created with default umask permissions (typical... |
| M5 | cmd_stop sends SIGKILL without confirmation after 2-second timeout | `apps/llm-proxy/src/main.rs:615` | In cmd_stop() (line 603), after sending SIGTERM and waiting 10 iterations of 200ms (2 seconds total), the code escalates to SIGKILL (line 636). This force-kills the process without any user confirm... |
| M6 | Forbidden header list is incomplete -- missing hop-by-hop and sensitive headers | `crates/llm-proxy-core/src/provider_config.rs:748` | The forbidden header list at line 748-752 only blocks `host`, `content-length`, `transfer-encoding`, and `connection`. It does not block other sensitive headers that can cause security issues when ... |
| M7 | Stream error message forwarded verbatim to client-facing SSE event | `crates/llm-proxy-protocol/src/client/anthropic.rs:784` | In encode_event's Error arm, error.message() is embedded directly into the ApiError message field that flows to the client. The code comments acknowledge this trust boundary (line 786-794) and the ... |
| M8 | Provider name used directly in filesystem path construction without additional validation in load_disk_cache | `crates/llm-proxy-server/src/catalog_service.rs:193` | Line 193 constructs a path using `format!("{}.toml", provider.name)`. While `write_catalog_atomic` validates the provider name via `is_safe_provider_slug`, the `load_disk_cache` function does not p... |
| M9 | get_client_ip returns unvalidated header values as IP addresses | `crates/llm-proxy-server/src/middleware.rs:276` | When trust_forwarded_headers is true, the X-Forwarded-For and X-Real-IP header values are extracted and returned as-is without validating that they are valid IP addresses. A malicious client could ... |
| M10 | X-Forwarded-For header value is not validated as a valid IP address | `crates/llm-proxy-server/src/middleware.rs:283` | At line 285, the leftmost comma-separated value from `X-Forwarded-For` is trimmed and returned directly as the client IP string without validating that it is a well-formed IP address. A malicious h... |
| M11 | Request body hashed for deduplication without size check | `crates/llm-proxy-server/src/routes/core_pipeline.rs:226` | In `prepare_request` (line 237), `state.request_dedup.is_duplicate_with_path(path, body)` hashes the entire request body via SHA-256. This is called after the body has already been limited to 32 Mi... |
| M12 | Rate limiter bypass for loopback addresses is IP-based only, not authenticated | `crates/llm-proxy-server/src/routes/core_pipeline.rs:229` | The `direct_loopback` check at line 229-230 bypasses rate limiting entirely when `trust_forwarded_headers` is false and the connection originates from a loopback address. Any process on the same ma... |
| M13 | RouteError::Upstream body can forward arbitrary upstream content to clients with only truncation | `crates/llm-proxy-server/src/routes/error_response.rs:306` | The `Upstream` variant's `body` field is passed through to the client response via `truncate_error_body` (line 309), which only truncates to 512 bytes. In the `map_catalog_error` function in models... |

### Error Handling

| # | Title | File:Line | Description |
|---|-------|-----------|-------------|
| M1 | libc::kill error checking loses errno information | `/apps/llm-proxy/src/main.rs:615` | After calling `libc::kill(pid as i32, libc::SIGTERM)` on line 615 and `libc::kill(pid as i32, libc::SIGKILL)` on line 636, the code checks `ret != 0` and bails with a generic message. It does not c... |
| M2 | Daemon child detachment is fragile -- try_wait error is only warned, not fatal | `apps/llm-proxy/src/main.rs:594` | At line 595, `child.try_wait()` failure is logged as a warning but otherwise ignored. If the child fails to start (e.g., executable removed between spawn and exec), the parent reports success and e... |
| M3 | Malformed tool_call.function.arguments silently replaced with empty object instead of returning an error | `crates/llm-proxy-protocol/src/client/openai_chat.rs:134` | At line 134, when `serde_json::from_str` fails to parse tool_call arguments, the code logs a warning and replaces the arguments with an empty JSON object `{}`. This silently discards potentially im... |
| M4 | Default impl uses expect() which panics on Client builder failure | `crates/llm-proxy-provider/src/discovery.rs:246` | The `Default` implementation for `DiscoveryClient` on line 246 calls `Self::try_new().expect(...)`. While `Client::builder().build()` is unlikely to fail in practice, it can fail if the TLS backend... |
| M5 | Cached refresh error is re-emitted as ProviderError::InvalidConfig losing original error type | `crates/llm-proxy-server/src/catalog_service.rs:109` | Line 107-109 converts a previously-cached error string back into a ProviderError::InvalidConfig. The original error could have been ProviderError::Api, ProviderError::Http, etc. After this conversi... |
| M6 | spawn_blocking JoinError silently discards panic information | `crates/llm-proxy-server/src/catalog_service.rs:130` | Line 130-136 maps JoinError to a ProviderError::InvalidConfig string. This means if the blocking task panics (e.g., due to a bug in serialization or filesystem operations), the panic payload is los... |
| M7 | expect() will panic if SIGTERM handler installation fails | `crates/llm-proxy-server/src/shutdown.rs:13` | Line 13-14 uses `.expect("failed to install SIGTERM handler")` on `signal::unix::signal()`. This can fail if the process has exceeded its file descriptor limit (the signal pipe uses an internal fd)... |

### Performance

| # | Title | File:Line | Description |
|---|-------|-----------|-------------|
| M1 | tokio::main uses default multi-threaded runtime for all commands | `apps/llm-proxy/src/main.rs:423` | The `#[tokio::main]` attribute on line 423 creates a multi-threaded runtime for *all* commands including synchronous ones (stop, status, init, validate, autostart). This adds unnecessary startup la... |
| M2 | glob_matches compiles a fresh Regex on every single call | `crates/llm-proxy-core/src/catalog.rs:80` | Every call to `glob_matches` allocates a new String, escapes characters, compiles a full Regex, runs the match, and then drops the Regex. This function is called O(allow.len() + deny.len()) times p... |
| M3 | Unnecessary heap allocation on every parsed line | `crates/llm-proxy-provider/src/sse.rs:101` | `line_bytes` is extracted via `self.buffer[..nl_pos].to_vec()`, creating a Vec<u8> allocation for every line. The bytes are immediately trimmed (borrowing the Vec) and then decoded to &str. The int... |
| M4 | RateLimiter stores per-IP buckets with String keys and prunes on every request | `crates/llm-proxy-server/src/middleware.rs:159` | RateLimiter uses a HashMap<String, ClientTokenBucket> with pruning on every is_allowed call (line 193). Each key is an IP address string. Under high load with many unique IPs, this creates GC press... |
| M5 | RateLimiter evicts stale entries on every single request | `crates/llm-proxy-server/src/middleware.rs:193` | Similar to the deduplicator, `buckets.retain(...)` at line 193 iterates all buckets on every `is_allowed()` call. Under high load with many distinct client IPs, this is O(n) per request. |
| M6 | SHA-256 hash computed on every request body for deduplication, even when window is zero | `crates/llm-proxy-server/src/routes/core_pipeline.rs:237` | The `is_duplicate_with_path` call at line 237 will compute a SHA-256 hash of the entire request body (up to 32 MiB per MAX_BODY_BYTES) on every request. When `dedup_window` is zero (the default per... |
| M7 | Fixed 256-buffer SSE channel may cause backpressure issues | `crates/llm-proxy-server/src/routes/core_pipeline.rs:809` | The mpsc channel on line 809 has a fixed buffer of 256 events. Under high-throughput streaming scenarios (e.g., a very chatty upstream producing many small events), the buffer could fill up, causin... |
| M8 | Fixed 256-event mpsc channel buffer has no backpressure mechanism | `crates/llm-proxy-server/src/routes/core_pipeline.rs:809` | At line 809, a `tokio::sync::mpsc::channel::<Event>(256)` is created. The spawned task (line 813) can fill this buffer with up to 256 SSE events before the consumer processes them. For a slow consu... |
| M9 | Oversized body test allocates 33 MiB unconditionally | `crates/llm-proxy-server/tests/chat_completions.rs:1364` | The test at line 1361 creates a 33 MiB string with 'X'.repeat(33 * 1024 * 1024) and sends it through the entire router pipeline. This allocates 33 MiB of heap memory on every test run and exercises... |

### Api Design

| # | Title | File:Line | Description |
|---|-------|-----------|-------------|
| M1 | ConfigValidation and ProviderResolution variants store only a free-form String message, discarding structured error context | `crates/llm-proxy-core/src/error.rs:22` | Both ConfigValidation and ProviderResolution variants store a single `message: String` field. Downstream code cannot programmatically inspect what kind of validation failed (e.g. empty api_key vs i... |
| M2 | EnvVarGuard is dead code -- defined but never used | `crates/llm-proxy-core/src/test_support.rs:46` | The EnvVarGuard struct (lines 46-98) is never referenced anywhere in the codebase. The only consumer of TestEnvLock (env_interpolate.rs:79) acquires the mutex but then calls std::env::set_var/remov... |
| M3 | Image block encode uses workaround via new_text() then manual field mutation instead of a proper constructor | `crates/llm-proxy-protocol/src/client/anthropic.rs:388` | encode_content_block encodes Image by calling ContentBlock::new_text(String::new()), then overwriting r#type to "image", clearing text, and setting source. This is fragile: if new_text() ever adds ... |
| M4 | ToolResult block encode uses workaround via new_text() then manual field mutation instead of a proper constructor | `crates/llm-proxy-protocol/src/client/anthropic.rs:417` | Same pattern as Image: tool_result encoding calls ContentBlock::new_text(String::new()), then manually overwrites multiple fields. This is fragile and inconsistent with the dedicated constructors f... |
| M5 | No validation on SamplingOptions numeric fields (temperature, top_p, max_tokens) | `crates/llm-proxy-protocol/src/core.rs:493` | The fields `temperature`, `top_p`, and `max_tokens` are plain `Option<f64>` / `Option<i32>` with no validation at deserialization or construction time. Negative temperatures (tested at line 2078), ... |
| M6 | max_tokens is modeled but max_completion_tokens is not | `crates/llm-proxy-protocol/src/openai.rs:175` | OpenAI's newer API versions use `max_completion_tokens` instead of `max_tokens`. The struct only has `max_tokens` (line 175). Clients sending `max_completion_tokens` will have the value silently dr... |
| M7 | is_safe_provider_slug does not enforce a maximum length | `crates/llm-proxy-server/src/catalog_service.rs:290` | The slug validation at line 290-295 checks for non-empty and ASCII lowercase/digit/hyphen/underscore characters, but does not enforce a maximum length. An extremely long provider name would result ... |
| M8 | Synthetic message ID never matches the upstream provider's real ID | `crates/llm-proxy-server/src/routes/core_pipeline.rs:614` | At lines 614-617, a synthetic message ID is generated (e.g., `chatcmpl-{uuid}` or `msg_{uuid}`) because the encoder needs an ID at construction time. The upstream provider's real message ID (availa... |
| M9 | ready() endpoint is a trivial tautology with no actual readiness check | `crates/llm-proxy-server/src/routes/health.rs:68` | The `ready()` handler always returns 200 OK regardless of actual system state (database connections, upstream provider reachability, memory pressure, etc.). The doc comment acknowledges this ('Will... |
| M10 | Fragile path-prefix heuristic for protocol detection in not_found fallback | `crates/llm-proxy-server/src/routes/mod.rs:127` | The not_found handler uses `path.contains("/v1/chat/")` to decide between OpenAI and Anthropic error shapes. This heuristic has several failure modes: (1) a request to `/v1/chat/` without `/provide... |
| M11 | Hardcoded `created: 0` timestamp in all model cards | `crates/llm-proxy-server/src/routes/models.rs:122` | Every ModelCard always sets `created: 0` (line 122) and `created_at: None` (line 126). OpenAI SDKs may sort or filter on this field, and a zero timestamp is indistinguishable from missing data. For... |
| M12 | Provider existence is checked but never used for token counting | `crates/llm-proxy-server/src/routes/token_count.rs:59` | Lines 58-61 validate the provider name and check that it exists in the registry, but the token counting logic does not use any provider-specific configuration (no tokenizer selection, no model-spec... |

### Maintainability

| # | Title | File:Line | Description |
|---|-------|-----------|-------------|
| M1 | main.rs is 1,634 lines with 41 functions -- should be decomposed into modules | `apps/llm-proxy/src/main.rs:1` | The file contains CLI definitions, PID management, config resolution, TOML constants, platform-specific autostart logic, command handlers, XML/desktop-entry formatting, home directory resolution, f... |
| M2 | 1,634-line main.rs mixes CLI parsing, config loading, server startup, daemon management, PID files, autostart, and platform-specific helpers | `apps/llm-proxy/src/main.rs:1` | The entire binary is in a single main.rs file spanning CLI definitions (lines 36-117), default TOML constants (123-214), path resolution (220-314), PID file management (320-391), command implementa... |
| M3 | resolve_serve_config and resolve_config are identical dead clones | `apps/llm-proxy/src/main.rs:248` | resolve_serve_config (lines 248-260) and resolve_config (lines 268-278) have identical bodies -- both check CLI path, then LLM_PROXY_CONFIG env var, then default_config_path(). The only difference ... |
| M4 | Duplicated PID management logic -- PidManager in core::pid is unused | `apps/llm-proxy/src/main.rs:321` | main.rs re-implements read_pid(), write_pid(), remove_pid(), and is_process_running() as free functions (lines 321-391) that are near-identical to the PidManager struct in crates/llm-proxy-core/src... |
| M5 | 1,634-line monolithic main.rs should be decomposed into modules | `apps/llm-proxy/src/main.rs:460` | The binary entrypoint at apps/llm-proxy/src/main.rs contains 1,634 lines with 30+ functions covering: CLI definitions, config loading, PID management, process management, daemon spawning, autostart... |
| M6 | Adapter header validation logic is duplicated for discovery headers | `crates/llm-proxy-core/src/provider_config.rs:731` | The header validation logic (empty name check, CRLF check, forbidden header check) at lines 731-759 for adapter headers is repeated almost identically at lines 777-799 for discovery headers. The fo... |
| M7 | ChatMessage intentionally lacks deny_unknown_fields but has no flattened extra map to capture dropped fields | `crates/llm-proxy-protocol/src/openai.rs:111` | The doc comment at lines 107-109 explains that deny_unknown_fields is omitted because ChatMessage is used in streaming deltas with varying fields. However, unlike ChatCompletionRequest (which has a... |
| M8 | Duplicated endpoint_without_query() function | `crates/llm-proxy-provider/src/adapter/mod.rs:130` | The function endpoint_without_query() is defined identically in both adapter/mod.rs (line 130) and transport.rs (line 94). Both functions strip query parameters from a URL for Debug redaction purpo... |
| M9 | Duplicated endpoint_without_query function in mod.rs and transport.rs | `crates/llm-proxy-provider/src/adapter/mod.rs:130` | The function endpoint_without_query is defined identically at line 130 in mod.rs and at line 94 in transport.rs. Both perform the same split_once('?') logic for Debug redaction. This duplication ri... |
| M10 | Anthropic version header "2023-06-01" hardcoded in two separate locations | `crates/llm-proxy-provider/src/adapter/mod.rs:411` | The Anthropic API version string "2023-06-01" is hardcoded both here at line 411 and in discovery.rs at lines 14-15 (DEFAULT_ANTHROPIC_VERSION). If the version needs to be updated, both locations m... |
| M11 | Duplicated stop_reason inference logic between response.completed and response.done handlers | `crates/llm-proxy-provider/src/adapter/responses.rs:214` | The stop_reason inference logic (checking for function_call outputs in chunk.output with fallback to saw_tool_call) is duplicated nearly verbatim between the 'response.completed' handler (lines 215... |
| M12 | Duplicated stop reason computation in response.completed and response.done | `crates/llm-proxy-provider/src/adapter/responses.rs:215` | The stop reason computation logic (approximately 17 lines) is duplicated verbatim between the response.completed handler (lines 215-231) and the response.done handler (lines 254-270). Both blocks c... |
| M13 | Error logging uses info! level instead of warn! for request failures | `crates/llm-proxy-server/src/routes/chat.rs:39` | Line 39 logs request failures at `info!` level. Client errors (400 Bad Request, 404 Not Found) at info level are reasonable, but upstream failures (502 Bad Gateway, 504 Gateway Timeout) and interna... |
| M14 | MAX_BODY_BYTES is a module-level magic constant with no config override | `crates/llm-proxy-server/src/routes/mod.rs:27` | The 32 MiB body limit is hardcoded as a module-level constant. There is no way to override it from configuration. For a proxy handling varying model context sizes, operators may need to tune this. |
| M15 | Manual Debug impl drift risk is mitigated by tests but not by the type system | `crates/llm-proxy-server/src/state.rs:50` | AppState uses a manual Debug impl (lines 77-103) for security reasons (documented redaction chain). The `#[non_exhaustive]` attribute (line 51) means adding a new field is a compile error only for ... |

### Test Coverage

| # | Title | File:Line | Description |
|---|-------|-----------|-------------|
| M1 | No integration tests for CLI binary -- commands like init, stop, status, serve, validate are untested as processes | `apps/llm-proxy/src/main.rs:1` | The binary crate has zero integration test files (no `apps/llm-proxy/tests/` directory exists). The unit tests in main.rs (lines 1394-1634) test only `models` subcommand logic and `catalog_enforcem... |
| M2 | Only 3 of 9 CLI commands have any test coverage; no integration tests for serve/stop/init/validate/autostart | `apps/llm-proxy/src/main.rs:460` | The test module (lines 1393-1634) covers: CLI flag parsing for models, catalog enforcement warnings, live model discovery, and live-discovery-with-static-catalog. There are zero tests for: cmd_serv... |
| M3 | No tests for PID file lifecycle, cmd_stop, cmd_status, cmd_init, daemon spawning, or signal handling | `apps/llm-proxy/src/main.rs:1393` | The test module (lines 1393-1634) covers only CLI flag parsing, catalog enforcement warnings, and live model discovery. There are zero tests for: (1) PID file creation/stale cleanup/removal, (2) cm... |
| M4 | No integration tests for CLI argument parsing edge cases | `apps/llm-proxy/src/main.rs:1393` | The test module (lines 1393-1634) has 5 tests: 1 CLI parsing test, 3 catalog/enforcement tests, and 1 live discovery test. Missing test coverage includes: (a) commands without --config use the env ... |
| M5 | No tests for parse_catalog_file with invalid TOML input | `crates/llm-proxy-core/src/catalog.rs:94` | The `parse_catalog_file` function is a public API that can fail on malformed input, but there is no test for the error path. Only the happy round-trip path is tested in `catalog_file_uses_nested_ca... |
| M6 | No tests for Discovered-only and Static-only catalog modes | `crates/llm-proxy-core/src/catalog.rs:94` | Only the `Hybrid` mode is tested via `hybrid_static_metadata_wins`. The `Discovered` and `Static` modes have no test coverage, meaning regressions in those branches would go undetected. |
| M7 | No test for load_from_dir | `crates/llm-proxy-core/src/provider_registry.rs:437` | The `load_from_dir` method (line 159) is a core public API with complex logic (symlink skipping, non-regular file filtering, duplicate name detection, sorted loading), but it has zero tests in this... |
| M8 | No test for MissingAdapter error variant | `crates/llm-proxy-core/src/provider_registry.rs:437` | The `ProviderRouteResolutionError::MissingAdapter` variant (line 56) is never exercised in tests. This occurs when a route references an adapter name that does not exist in the provider's adapter m... |
| M9 | Encode test constructs CoreResponse from output.json, then compares encode output to the same output.json -- a tautological round-trip | `crates/llm-proxy-protocol/tests/fixture_tests.rs:510` | Lines 499-514 and the comment at lines 499-509 acknowledge that `build_core_response_from_output` reads output.json, then `assert_encode_matches_output` compares the encoded result against the same... |
| M10 | Streaming fixture tests only validate fixture structure, not actual adapter encode/decode behavior | `crates/llm-proxy-protocol/tests/fixture_tests.rs:862` | Lines 862-886 (streaming_core_events_json_is_valid) only verify that core-events.json parses as Vec<CoreEvent>. Lines 892-938 only verify SSE format. The streaming_encode_round_trip test (line 957)... |
| M11 | No test for cache_is_fresh logic | `crates/llm-proxy-server/src/catalog_service.rs:405` | The cache_is_fresh method (line 167) parses RFC 3339 timestamps and compares against the TTL, but there is no unit test directly exercising this logic, including edge cases like expired entries, fu... |
| M12 | No tests for RateLimiter actually rejecting requests at the limit | `crates/llm-proxy-server/src/middleware.rs:317` | The test `rate_limiter_allows_under_limit` (line 362) verifies requests below the limit are allowed, but no test verifies that requests *exceeding* the limit are actually rejected. The `rate_limite... |
| M13 | No tests for concurrent access to RequestDeduplicator or RateLimiter | `crates/llm-proxy-server/src/middleware.rs:406` | Both `RequestDeduplicator` and `RateLimiter` use `Mutex<HashMap<...>>` intended for concurrent access in an async Axum server, but all tests are single-threaded unit tests. There are no multi-threa... |
| M14 | Regression test `app_state_debug_covers_all_fields` omits `model_catalogs` from its field list | `crates/llm-proxy-server/src/state.rs:309` | The test on line 296 is designed to catch fields added to AppState but missing from the manual Debug impl. The field list iterated on lines 309-319 includes ten fields but omits `model_catalogs`, w... |
| M15 | No test for request timeout behavior | `crates/llm-proxy-server/tests/chat_completions.rs:700` | The ServerConfig has a request_timeout field (set to 300s or 30s in tests) but no test verifies that a slow upstream causes a timeout response. There is no test for the tower timeout middleware int... |
| M16 | No test for concurrent request handling or rate limiting | `crates/llm-proxy-server/tests/chat_completions.rs:700` | The rate_limit_rpm is set to 100 in all test configs, but no test verifies that the rate limiter actually rejects requests exceeding the limit. There is also no test for concurrent request interlea... |
| M17 | No test for authentication / API key forwarding | `crates/llm-proxy-server/tests/chat_completions.rs:700` | The provider configs set api_key: "test-key" but no test verifies that the key is actually sent to the upstream mock as an Authorization header, or that requests without authentication to the proxy... |
| M18 | No test for cross-protocol non-streaming (Anthropic upstream -> OpenAI response) | `crates/llm-proxy-server/tests/chat_completions.rs:700` | The streaming tool-call test (line 783) tests cross-protocol streaming (Anthropic mock upstream, OpenAI response format), but there is no corresponding non-streaming cross-protocol test that sends ... |
| M19 | spawn_mock_server silently swallows serve errors with .unwrap() in spawned task | `crates/llm-proxy-server/tests/core_pipeline.rs:244` | Line 244: `tokio::spawn(async move { axum::serve(listener, app).await.unwrap() })` -- if the mock server crashes (e.g. port conflict, panic in handler), the unwrap() panics inside the spawned task ... |
| M20 | Test name contradicts its behavior: unknown_model_passes_through_to_upstream | `crates/llm-proxy-server/tests/core_pipeline.rs:644` | Lines 644-661: The test function is named `unknown_model_passes_through_to_upstream` but the doc comment on line 643 says 'unknown model returns 400 and does not call upstream'. The test actually v... |
| M21 | No integration test for /providers/{provider}/v1/chat/completions route | `crates/llm-proxy-server/tests/core_pipeline.rs:730` | The test file exercises `/providers/{provider}/v1/messages` extensively but has no test for the `/providers/{provider}/v1/chat/completions` route defined in routes/mod.rs line 84. The chat handler ... |
| M22 | No integration test for /health, /ready, /version endpoints | `crates/llm-proxy-server/tests/core_pipeline.rs:730` | The test file focuses exclusively on the messages route and its variants. The lightweight health/readiness/version endpoints (routes/health.rs) are not tested through the router. While they have no... |
| M23 | No integration test for /providers/{provider}/v1/models endpoint | `crates/llm-proxy-server/tests/core_pipeline.rs:730` | The models endpoint (routes/models.rs) is mounted in the router but has no integration test in this file. While models.rs has its own unit tests, an integration test through the router would verify... |
| M24 | No test for OpenAI Chat provider with non-streaming upstream error (502 mapping) | `crates/llm-proxy-server/tests/core_pipeline.rs:882` | The test file has `upstream_500_returns_502` (line 623) for the Anthropic provider but does not test that the OpenAI Chat provider route also maps upstream errors correctly. The chat completions ro... |
| M25 | Rate limit test is non-deterministic and cannot fail | `crates/llm-proxy-server/tests/core_pipeline.rs:1147` | Lines 1147-1181: The `rate_limited_request_returns_429` test sends 101 requests in a tight loop and checks if any returns 429. If none do, the test passes anyway with a comment saying 'the importan... |
| M26 | Dedup test is non-deterministic and cannot fail | `crates/llm-proxy-server/tests/core_pipeline.rs:1185` | Lines 1185-1215: The `duplicate_request_returns_409` test sends two identical requests but only checks IF the second returns 409 -- if it does not (e.g. the dedup window expired between requests), ... |
| M27 | No integration tests for request timeout (408) behavior | `crates/llm-proxy-server/tests/integration.rs:1` | The router in routes/mod.rs applies TimeoutLayer with StatusCode::REQUEST_TIMEOUT for API routes, but no integration test verifies that a slow upstream triggers a 408 response. The test configurati... |
| M28 | messages_requires_auth_header test only asserts not-404, does not verify auth enforcement | `crates/llm-proxy-server/tests/integration.rs:163` | The test at line 163 sends a request without an x-api-key header but only asserts `assert_ne!(resp.status(), StatusCode::NOT_FOUND)`. It never checks whether the server actually rejected the reques... |
| M29 | No integration tests for GET /providers/{provider}/v1/models route | `crates/llm-proxy-server/tests/integration.rs:184` | The /providers/{provider}/v1/models route is registered in routes/mod.rs (line 93) but integration.rs has no end-to-end tests for it. The models.rs handler has unit tests (handle_models_inner) but ... |
| M30 | toml_messages_passes_through_to_upstream does not verify model name passthrough | `crates/llm-proxy-server/tests/integration.rs:552` | The test at line 552 asserts the response is not 404 and is either 502 or 500, but does not verify that the model name was actually passed through to the upstream request. The test comment says 'th... |

---

## Systemic Patterns (Cross-Crate)

### Security Patterns

| Pattern | Locations | Impact |
|---------|-----------|--------|
| Error message leakage in SSE streaming | `client/anthropic.rs:784`, `adapter/anthropic.rs:494` | Upstream errors forwarded verbatim to clients |
| Forbidden headers incomplete | `provider_config.rs:748` | Missing `authorization`, `cookie`, `set-cookie`, `upgrade` |
| PID file default permissions | `main.rs:336` | World-readable by default |
| Daemon log default permissions | `main.rs:562` | Log may contain secrets, created with umask |
| Loopback rate-limit bypass | `core_pipeline.rs:229` | Any local process gets unlimited requests |
| X-Forwarded-For not validated as IP | `middleware.rs:276` | Arbitrary strings used as rate-limit keys |

### Correctness Patterns

| Pattern | Locations | Impact |
|---------|-----------|--------|
| Silent data loss for missing tool IDs | `client/anthropic.rs:193,264` | Empty-string IDs break tool round-tripping |
| TOCTOU races in PID files | `pid.rs:46,76`, `main.rs:483` | File existence checks before read/remove are racy |
| Empty stop array accepted as meaningful | `core.rs:502` | `Some(vec![])` is semantically meaningless |
| 2-second SIGKILL escalation | `main.rs:624` | Too short for draining streaming connections |
| `cmd_stop` no-op on non-Unix | `main.rs:603` | Prints success without stopping on Windows |

### Performance Patterns

| Pattern | Locations | Impact |
|---------|-----------|--------|
| Regex recompilation on every glob match | `catalog.rs:80` | O(patterns × models) compilation |
| SHA-256 on every request body | `core_pipeline.rs:237` | Even when dedup is disabled |
| Per-request String allocation in metrics | `metrics.rs:86` | Hot-path `format!` |
| Heap allocation per SSE line | `sse.rs:101` | `to_vec()` on every parsed line |
| O(n) prune on every rate-limit check | `middleware.rs:193` | `retain()` runs per request |
| String-keyed HashMap for IPs | `middleware.rs:159` | Allocation per lookup instead of `IpAddr` |

### Maintainability Patterns

| Pattern | Locations | Impact |
|---------|-----------|--------|
| 1,634-line monolithic `main.rs` | `apps/llm-proxy/src/main.rs` | Should be decomposed into modules |
| Duplicated `endpoint_without_query()` | `adapter/mod.rs:130`, `transport.rs:94` | Identical private functions |
| Anthropic version hardcoded in 2 places | `adapter/mod.rs:411`, `discovery.rs:14` | `"2023-06-01"` not shared |
| `PidManager` in core unused by binary | `core/pid.rs` vs `main.rs:321-391` | Binary re-implements with bugs |
| Duplicated header validation logic | `provider_config.rs:731,777` | Adapter and discovery headers share validation |

---

## Recommended Priority Order

1. **Fix PID file race** — atomic creation with `create_new(true)` + fix `EPERM` handling
2. **Fix rate limiter IP validation** — parse `IpAddr` from headers, cap bucket count
3. **Fix `ChatMessage.content` polymorphism** — support OpenAI content arrays
4. **Cap `RequestDeduplicator` HashMap** — prevent OOM DoS
5. **Add error message sanitization** in streaming SSE path
6. **Expand forbidden header list** — add `authorization`, `cookie`, `upgrade`, etc.
7. **Increase SIGKILL grace period** to 10-30 seconds (configurable)
8. **Decompose `main.rs`** into modules + use `PidManager` from core
9. **Add missing integration tests** for health, models, chat completions routes

---

## 🟡 LOW Findings (Compact)

355 low-severity findings. Top themes:

- **correctness** (66 findings)
- **maintainability** (59 findings)
- **test-coverage** (58 findings)
- **api-design** (41 findings)
- **performance** (39 findings)
- **security** (33 findings)
- **error-handling** (31 findings)
- **idiomatic-rust** (28 findings)

<details>
<summary>View all LOW findings</summary>

| # | Category | Title | File:Line |
|---|----------|-------|-----------|
| 1 | error-handling | serde_json::to_string failure silently returns None with no logging | `/crates/llm-proxy-server/src/routes/error_response.rs:277` |
| 2 | error-handling | map_catalog_error uses unwrap_or for invalid HTTP status codes, silently down... | `/crates/llm-proxy-server/src/routes/models.rs:163` |
| 3 | security | cmd_init writes config file before setting permissions -- brief window with d... | `apps/llm-proxy/apps/llm-proxy/src/main.rs:703` |
| 4 | maintainability | Three large inline TOML constants (~80 lines) should be include_str! from files | `apps/llm-proxy/src/main.rs:133` |
| 5 | correctness | config_dir falls back to relative path (./) when HOME is unset | `apps/llm-proxy/src/main.rs:225` |
| 6 | maintainability | resolve_serve_config and resolve_config are identical -- dead duplication | `apps/llm-proxy/src/main.rs:248` |
| 7 | maintainability | resolve_serve_config and resolve_config are identical functions with differen... | `apps/llm-proxy/src/main.rs:248` |
| 8 | security | PID file created with default umask permissions before restrictive chmod | `apps/llm-proxy/src/main.rs:342` |
| 9 | security | PID file created with default (world-readable) permissions | `apps/llm-proxy/src/main.rs:342` |
| 10 | correctness | is_process_running accepts PID 0, which sends signal to entire process group | `apps/llm-proxy/src/main.rs:366` |
| 11 | correctness | is_process_running diverges from core::pid::PidManager -- EPERM handling differs | `apps/llm-proxy/src/main.rs:366` |
| 12 | correctness | is_process_running accepts PID 0 which would check all-process-group on some ... | `apps/llm-proxy/src/main.rs:366` |
| 13 | correctness | libc::kill() with pid as i32 truncation on 64-bit systems is harmless but the... | `apps/llm-proxy/src/main.rs:372` |
| 14 | idiomatic-rust | Raw libc unsafe calls could be replaced by nix or rustix crate wrappers | `apps/llm-proxy/src/main.rs:372` |
| 15 | error-handling | errno read on non-macOS/non-Linux Unix always returns false for EPERM case | `apps/llm-proxy/src/main.rs:381` |
| 16 | correctness | Non-Unix is_process_running always returns true -- makes stop/status unreliable | `apps/llm-proxy/src/main.rs:382` |
| 17 | performance | Multi-threaded tokio runtime used for primarily single-listen server | `apps/llm-proxy/src/main.rs:423` |
| 18 | performance | main() uses #[tokio::main] (multi-threaded runtime) for commands that are pur... | `apps/llm-proxy/src/main.rs:424` |
| 19 | error-handling | main returns Result but does not set explicit exit codes | `apps/llm-proxy/src/main.rs:424` |
| 20 | performance | main uses #[tokio::main] for commands that are entirely synchronous | `apps/llm-proxy/src/main.rs:424` |
| 21 | security | Hidden --daemonize flag allows unauthenticated local privilege escalation via... | `apps/llm-proxy/src/main.rs:467` |
| 22 | correctness | init_tracing() called after daemonization check -- daemon mode logs nothing d... | `apps/llm-proxy/src/main.rs:471` |
| 23 | correctness | PID cleanup uses a separate async block that shadows the synchronous remove_p... | `apps/llm-proxy/src/main.rs:498` |
| 24 | correctness | PID cleanup uses a captured path that may not match the actual PID file if co... | `apps/llm-proxy/src/main.rs:499` |
| 25 | security | Daemon log file opened with O_APPEND but no log rotation -- unbounded growth | `apps/llm-proxy/src/main.rs:559` |
| 26 | error-handling | Daemon log file opened in append mode with no size limit or rotation | `apps/llm-proxy/src/main.rs:563` |
| 27 | correctness | Daemon log file opened with append but may grow unbounded | `apps/llm-proxy/src/main.rs:563` |
| 28 | correctness | child.try_wait() return value is misinterpreted -- error is treated as non-fa... | `apps/llm-proxy/src/main.rs:594` |
| 29 | correctness | spawn_daemon does not propagate errors from daemon child startup | `apps/llm-proxy/src/main.rs:594` |
| 30 | error-handling | spawn_daemon uses tracing::warn but tracing is not initialized -- log line is... | `apps/llm-proxy/src/main.rs:596` |
| 31 | correctness | cmd_stop uses raw libc kill on non-Unix builds but is gated with #[cfg(unix)]... | `apps/llm-proxy/src/main.rs:614` |
| 32 | correctness | cmd_stop prints 'sent SIGTERM to PID' even on non-Unix where no signal is act... | `apps/llm-proxy/src/main.rs:620` |
| 33 | correctness | cmd_stop polling loop uses std::thread::sleep in async context | `apps/llm-proxy/src/main.rs:624` |
| 34 | maintainability | Magic numbers in stop polling loop: 10 iterations and 200ms sleep | `apps/llm-proxy/src/main.rs:630` |
| 35 | maintainability | cmd_status uses default_config_path instead of the actual resolved config path | `apps/llm-proxy/src/main.rs:659` |
| 36 | correctness | cmd_status always loads config from default path, ignoring custom config loca... | `apps/llm-proxy/src/main.rs:659` |
| 37 | security | Config directory created without restrictive permissions | `apps/llm-proxy/src/main.rs:699` |
| 38 | security | cmd_init creates files with default permissions then chmods -- race window | `apps/llm-proxy/src/main.rs:703` |
| 39 | error-handling | RFC3339 timestamp formatting unwrap_or fallback to epoch is silently misleading | `apps/llm-proxy/src/main.rs:977` |
| 40 | error-handling | launchctl unload failure is silently ignored in cmd_autostart_disable | `apps/llm-proxy/src/main.rs:1084` |
| 41 | error-handling | launchctl unload failure silently ignored in cmd_autostart_disable | `apps/llm-proxy/src/main.rs:1095` |
| 42 | security | format_plist wraps command in bash -c without proper argument separation | `apps/llm-proxy/src/main.rs:1157` |
| 43 | security | format_plist passes command through /bin/bash -c shell invocation | `apps/llm-proxy/src/main.rs:1157` |
| 44 | security | format_desktop_entry escaping is insufficient for Exec key | `apps/llm-proxy/src/main.rs:1197` |
| 45 | security | Desktop entry escaping is incomplete for Exec key | `apps/llm-proxy/src/main.rs:1197` |
| 46 | test-coverage | xml_escape function has no unit tests | `apps/llm-proxy/src/main.rs:1212` |
| 47 | security | catalog_enforcement_warning follows symlinks when reading cached catalog | `apps/llm-proxy/src/main.rs:1305` |
| 48 | maintainability | init_tracing ignores the config file's log_level setting | `apps/llm-proxy/src/main.rs:1333` |
| 49 | maintainability | llm-proxy-api stub crate is empty with no re-exports | `crates/llm-proxy-api/src/lib.rs:1` |
| 50 | error-handling | parse_catalog_file error loses filename context | `crates/llm-proxy-core/src/catalog.rs:35` |
| 51 | maintainability | model_allowed uses raw String slices for allow/deny instead of pre-compiled p... | `crates/llm-proxy-core/src/catalog.rs:71` |
| 52 | performance | glob_matches compiles a new Regex on every call | `crates/llm-proxy-core/src/catalog.rs:80` |
| 53 | performance | Inefficient per-character allocation in glob_matches regex builder | `crates/llm-proxy-core/src/catalog.rs:87` |
| 54 | test-coverage | No test for glob_matches with special regex characters in model IDs | `crates/llm-proxy-core/src/catalog.rs:94` |
| 55 | test-coverage | hybrid_static_metadata_wins test relies on BTreeMap sort order with magic index | `crates/llm-proxy-core/src/catalog.rs:126` |
| 56 | api-design | interpolate_env_vars is public but does not report which variables were resol... | `crates/llm-proxy-core/src/env_interpolate.rs:36` |
| 57 | api-design | find_unresolved_env_var returns only the first unresolved variable, not all | `crates/llm-proxy-core/src/env_interpolate.rs:48` |
| 58 | performance | find_env_var_refs allocates a String for every match even when the caller onl... | `crates/llm-proxy-core/src/env_interpolate.rs:58` |
| 59 | test-coverage | No test for find_env_var_refs function | `crates/llm-proxy-core/src/env_interpolate.rs:73` |
| 60 | test-coverage | No test for multiple interpolations in a single string | `crates/llm-proxy-core/src/env_interpolate.rs:73` |
| 61 | test-coverage | No test for interpolation of a variable set to empty string | `crates/llm-proxy-core/src/env_interpolate.rs:78` |
| 62 | maintainability | Test uses raw unsafe set_var/remove_var instead of EnvVarGuard | `crates/llm-proxy-core/src/env_interpolate.rs:80` |
| 63 | performance | regex::escape called per-character in glob_matches | `crates/llm-proxy-core/src/env_interpolate.rs:87` |
| 64 | api-design | CoreError lacks std::error::Error and From implementations for crate-internal... | `crates/llm-proxy-core/src/error.rs:8` |
| 65 | api-design | CoreError does not implement Clone | `crates/llm-proxy-core/src/error.rs:8` |
| 66 | test-coverage | No direct unit tests for CoreError variants | `crates/llm-proxy-core/src/error.rs:8` |
| 67 | correctness | HashMap<String, AtomicI64> stored inside Mutex is a thread-safety footgun | `crates/llm-proxy-core/src/metrics.rs:38` |
| 68 | error-handling | Silently swallowed Mutex poison errors in record_success and get_snapshot | `crates/llm-proxy-core/src/metrics.rs:78` |
| 69 | error-handling | Silently swallowing Mutex poisoning in record_success | `crates/llm-proxy-core/src/metrics.rs:78` |
| 70 | performance | Per-request heap allocation in record_success for model key formatting | `crates/llm-proxy-core/src/metrics.rs:86` |
| 71 | idiomatic-rust | model_counts key uses a String separator, which could collide with provider o... | `crates/llm-proxy-core/src/metrics.rs:86` |
| 72 | performance | String allocation on every record_success call for the composite key | `crates/llm-proxy-core/src/metrics.rs:86` |
| 73 | api-design | record_failure does not track which provider/model failed | `crates/llm-proxy-core/src/metrics.rs:100` |
| 74 | maintainability | Snapshot is not a consistent point-in-time view across mutex and atomics | `crates/llm-proxy-core/src/metrics.rs:117` |
| 75 | api-design | Snapshot exposes raw latencies Vec but only exposes p95/p99 as methods | `crates/llm-proxy-core/src/metrics.rs:188` |
| 76 | performance | percentile() allocates a sorted copy on every call | `crates/llm-proxy-core/src/metrics.rs:219` |
| 77 | performance | percentile() allocates and sorts a full Vec on every call | `crates/llm-proxy-core/src/metrics.rs:219` |
| 78 | test-coverage | No test for Snapshot's percentile behavior with a single sample | `crates/llm-proxy-core/src/metrics.rs:337` |
| 79 | test-coverage | No test for get_snapshot under concurrent mutation | `crates/llm-proxy-core/src/metrics.rs:337` |
| 80 | test-coverage | No test for metrics key separator collision | `crates/llm-proxy-core/src/metrics.rs:337` |
| 81 | api-design | PidManager uses anyhow::Result while rest of crate uses CoreError | `crates/llm-proxy-core/src/pid.rs:1` |
| 82 | maintainability | PID file name 'llm-proxy.pid' is hardcoded, not configurable | `crates/llm-proxy-core/src/pid.rs:25` |
| 83 | correctness | write_pid does not write PID atomically, risking readers seeing empty/partial... | `crates/llm-proxy-core/src/pid.rs:61` |
| 84 | security | PID file created with default (umask-dependent) permissions, no restrictive m... | `crates/llm-proxy-core/src/pid.rs:65` |
| 85 | error-handling | TOCTOU race between pid_file.exists() and remove_file in remove_pid | `crates/llm-proxy-core/src/pid.rs:75` |
| 86 | test-coverage | No test for remove_pid functionality | `crates/llm-proxy-core/src/pid.rs:75` |
| 87 | api-design | is_process_running always returns true on non-Unix, making it unreliable | `crates/llm-proxy-core/src/pid.rs:88` |
| 88 | error-handling | is_process_running silently treats EPERM as 'not running' | `crates/llm-proxy-core/src/pid.rs:93` |
| 89 | maintainability | rate_limit_rpm uses u32 but a value of 0 has special semantics (disabled) | `crates/llm-proxy-core/src/provider_config.rs:50` |
| 90 | api-design | api_key uses skip_serializing, breaking TOML round-trips | `crates/llm-proxy-core/src/provider_config.rs:112` |
| 91 | api-design | ProviderAdapterConfig exposes endpoint with query parameters that may contain... | `crates/llm-proxy-core/src/provider_config.rs:173` |
| 92 | maintainability | endpoint_without_query is duplicated three times across crates | `crates/llm-proxy-core/src/provider_config.rs:195` |
| 93 | api-design | ProviderDiscoveryConfig.endpoint also lacks sensitivity documentation | `crates/llm-proxy-core/src/provider_config.rs:292` |
| 94 | security | Provider name slug regex does not enforce a length limit | `crates/llm-proxy-core/src/provider_config.rs:669` |
| 95 | maintainability | Forbidden header list is duplicated between adapter and discovery validation | `crates/llm-proxy-core/src/provider_config.rs:748` |
| 96 | correctness | Gemini template detection uses string matching instead of protocol-aware check | `crates/llm-proxy-core/src/provider_config.rs:816` |
| 97 | correctness | Route adapter name lookup uses trimmed name but HashMap key may not be trimmed | `crates/llm-proxy-core/src/provider_config.rs:837` |
| 98 | correctness | Route adapter cross-reference lookup uses trimmed key against untrimmed HashM... | `crates/llm-proxy-core/src/provider_config.rs:845` |
| 99 | test-coverage | load_app_config checks for empty env vars but no test covers this path | `crates/llm-proxy-core/src/provider_config.rs:939` |
| 100 | api-design | load_from_dir has a misplaced #[must_use] attribute | `crates/llm-proxy-core/src/provider_registry.rs:141` |
| 101 | maintainability | #[must_use] placed on a Result-returning function is misleading | `crates/llm-proxy-core/src/provider_registry.rs:141` |
| 102 | security | Symlink check uses Path::is_symlink which may behave unexpectedly on some pla... | `crates/llm-proxy-core/src/provider_registry.rs:189` |
| 103 | correctness | file_type() failure silently treated as regular file | `crates/llm-proxy-core/src/provider_registry.rs:198` |
| 104 | performance | Double HashMap lookup when inserting providers | `crates/llm-proxy-core/src/provider_registry.rs:234` |
| 105 | maintainability | Route kind name mapping is a manual match that must be kept in sync with Prov... | `crates/llm-proxy-core/src/provider_registry.rs:348` |
| 106 | maintainability | Magic number 5 for adapter list truncation threshold | `crates/llm-proxy-core/src/provider_registry.rs:360` |
| 107 | api-design | catalog_models returns Option<&[StaticModelCatalogEntry]> with a confusing No... | `crates/llm-proxy-core/src/provider_registry.rs:425` |
| 108 | test-coverage | No test for from_providers duplicate detection | `crates/llm-proxy-core/src/provider_registry.rs:437` |
| 109 | test-coverage | No test for is_empty() and len() | `crates/llm-proxy-core/src/provider_registry.rs:437` |
| 110 | test-coverage | No test for get() method | `crates/llm-proxy-core/src/provider_registry.rs:437` |
| 111 | test-coverage | No test for catalog_models() method | `crates/llm-proxy-core/src/provider_registry.rs:437` |
| 112 | test-coverage | No test for resolve_provider_route with model that has no alias | `crates/llm-proxy-core/src/provider_registry.rs:437` |
| 113 | test-coverage | No test for the adapter list truncation in MissingAdapter error | `crates/llm-proxy-core/src/provider_registry.rs:437` |
| 114 | api-design | Counter is a zero-sized type (ZST) but wrapped in Arc<Counter> at the call site | `crates/llm-proxy-core/src/token/counter.rs:32` |
| 115 | idiomatic-rust | Counter::new() is redundant with Default derive | `crates/llm-proxy-core/src/token/counter.rs:36` |
| 116 | maintainability | Magic constant 4 for chars-per-token ratio is not named | `crates/llm-proxy-core/src/token/counter.rs:44` |
| 117 | correctness | count_tokens uses byte length instead of character count for multilingual text | `crates/llm-proxy-core/src/token/counter.rs:49` |
| 118 | api-design | count_messages takes system as &str but could accept Option<&str> to signal o... | `crates/llm-proxy-core/src/token/counter.rs:65` |
| 119 | api-design | count_messages does not validate role strings | `crates/llm-proxy-core/src/token/counter.rs:65` |
| 120 | api-design | system prompt is passed as a flat &str but can contain multiple blocks in the... | `crates/llm-proxy-core/src/token/counter.rs:65` |
| 121 | test-coverage | No test for count_messages with empty role string | `crates/llm-proxy-core/src/token/counter.rs:65` |
| 122 | test-coverage | No test for count_messages with empty content in a message | `crates/llm-proxy-core/src/token/counter.rs:65` |
| 123 | maintainability | Magic constants BASE_TOKENS, PER_MESSAGE_OVERHEAD, SYSTEM_OVERHEAD lack deriv... | `crates/llm-proxy-core/src/token/counter.rs:66` |
| 124 | correctness | validate() does not check for negative max_tokens or missing required content... | `crates/llm-proxy-protocol/src/anthropic.rs:102` |
| 125 | security | Metadata struct lacks deny_unknown_fields, inconsistent with sibling structs | `crates/llm-proxy-protocol/src/anthropic.rs:158` |
| 126 | error-handling | Message::content_blocks silently drops unparseable array items | `crates/llm-proxy-protocol/src/anthropic.rs:228` |
| 127 | error-handling | content_blocks() silently drops unparseable array items without logging | `crates/llm-proxy-protocol/src/anthropic.rs:286` |
| 128 | api-design | get_tool_id() returns owned String when a &str reference would suffice | `crates/llm-proxy-protocol/src/anthropic.rs:401` |
| 129 | performance | tool_use serialization clones input unnecessarily | `crates/llm-proxy-protocol/src/anthropic.rs:469` |
| 130 | correctness | Custom Serialize for image block creates a throwaway ImageSource when source ... | `crates/llm-proxy-protocol/src/anthropic.rs:511` |
| 131 | idiomatic-rust | Unknown ContentBlock types re-serialize all 14 fields via AllFields struct | `crates/llm-proxy-protocol/src/anthropic.rs:525` |
| 132 | api-design | Usage fields use i32 which limits token counts to ~2.1 billion | `crates/llm-proxy-protocol/src/anthropic.rs:672` |
| 133 | idiomatic-rust | Delta.r#type is Option<String> but is always required by the API | `crates/llm-proxy-protocol/src/anthropic.rs:710` |
| 134 | performance | decode_system clones each JSON value for deserialization | `crates/llm-proxy-protocol/src/client/anthropic.rs:120` |
| 135 | performance | Unnecessary clone of serde_json::Value in decode_system array branch | `crates/llm-proxy-protocol/src/client/anthropic.rs:123` |
| 136 | security | String truncation in decode_system uses byte index on block.r#type | `crates/llm-proxy-protocol/src/client/anthropic.rs:136` |
| 137 | error-handling | Image source deserialization failure silently produces empty ImageSource | `crates/llm-proxy-protocol/src/client/anthropic.rs:188` |
| 138 | performance | Item clone inside tool_result array decode when inner deserialization is atte... | `crates/llm-proxy-protocol/src/client/anthropic.rs:214` |
| 139 | security | String truncation in decode_content_block error path uses byte index on poten... | `crates/llm-proxy-protocol/src/client/anthropic.rs:224` |
| 140 | security | String truncation for log output uses byte index which could panic on multi-b... | `crates/llm-proxy-protocol/src/client/anthropic.rs:281` |
| 141 | api-design | decode_tool_choice defaults missing tool name to empty string | `crates/llm-proxy-protocol/src/client/anthropic.rs:305` |
| 142 | error-handling | encode_content_block Image uses unwrap_or_else masking source deserialization... | `crates/llm-proxy-protocol/src/client/anthropic.rs:388` |
| 143 | maintainability | StreamEncoder silently swallows events after finished flag is set | `crates/llm-proxy-protocol/src/client/anthropic.rs:573` |
| 144 | performance | encode_event allocates Vec for every call even when returning 0 or 1 events | `crates/llm-proxy-protocol/src/client/anthropic.rs:577` |
| 145 | correctness | Tool messages always have is_error=false with no way to set it to true | `crates/llm-proxy-protocol/src/client/openai_chat.rs:174` |
| 146 | correctness | stop field parsing diverges from core.rs deserialize_stop: non-string non-arr... | `crates/llm-proxy-protocol/src/client/openai_chat.rs:218` |
| 147 | correctness | decode_tool_choice silently accepts tool_choice with empty function name | `crates/llm-proxy-protocol/src/client/openai_chat.rs:299` |
| 148 | maintainability | Non-deterministic UUID in encode_response prevents reproducible testing | `crates/llm-proxy-protocol/src/client/openai_chat.rs:349` |
| 149 | test-coverage | encode_response timestamp is non-deterministic, preventing snapshot tests | `crates/llm-proxy-protocol/src/client/openai_chat.rs:453` |
| 150 | correctness | encode_usage silently drops reasoning_tokens with a warning instead of includ... | `crates/llm-proxy-protocol/src/client/openai_chat.rs:497` |
| 151 | performance | MessageStart creates a new String for role on every event even though it is a... | `crates/llm-proxy-protocol/src/client/openai_chat.rs:586` |
| 152 | test-coverage | No test for decode_request with assistant message containing both reasoning_c... | `crates/llm-proxy-protocol/src/client/openai_chat.rs:862` |
| 153 | test-coverage | No test for StreamEncoder with multiple UsageDelta events (last-wins behavior) | `crates/llm-proxy-protocol/src/client/openai_chat.rs:862` |
| 154 | security | redact_value shows String char count, which is a partial information leak | `crates/llm-proxy-protocol/src/core.rs:63` |
| 155 | idiomatic-rust | Manual Debug impls could use derive with redacting wrappers for simpler types | `crates/llm-proxy-protocol/src/core.rs:150` |
| 156 | api-design | CoreContent enum uses #[serde(deny_unknown_fields)] which limits forward comp... | `crates/llm-proxy-protocol/src/core.rs:213` |
| 157 | security | CoreTool::input_schema is a bare serde_json::Value with no structural validation | `crates/llm-proxy-protocol/src/core.rs:418` |
| 158 | idiomatic-rust | StopReason::Unknown serializes as a string, making it indistinguishable from ... | `crates/llm-proxy-protocol/src/core.rs:689` |
| 159 | correctness | Usage token counts use i32, risking overflow on large responses | `crates/llm-proxy-protocol/src/core.rs:715` |
| 160 | api-design | CoreStreamErrorKind::http_status() takes self by value | `crates/llm-proxy-protocol/src/core.rs:1059` |
| 161 | correctness | NaN and Infinity silently lost during JSON round-trips | `crates/llm-proxy-protocol/src/core.rs:2089` |
| 162 | test-coverage | openai.rs has zero unit tests despite defining 13 public types | `crates/llm-proxy-protocol/src/openai.rs:1` |
| 163 | api-design | CacheControl uses an untyped String for `type` instead of an enum or newtype | `crates/llm-proxy-protocol/src/openai.rs:18` |
| 164 | idiomatic-rust | Duplicate CacheControl struct between openai and core modules | `crates/llm-proxy-protocol/src/openai.rs:19` |
| 165 | idiomatic-rust | Several structs lack PartialEq derive despite fields supporting it | `crates/llm-proxy-protocol/src/openai.rs:19` |
| 166 | api-design | ToolDef.r#type is an untyped String that must always be "function" | `crates/llm-proxy-protocol/src/openai.rs:42` |
| 167 | correctness | ToolCall.index is i32 but OpenAI spec uses non-negative integers | `crates/llm-proxy-protocol/src/openai.rs:74` |
| 168 | maintainability | ToolCall.index is i32 but content block indices in core are usize | `crates/llm-proxy-protocol/src/openai.rs:74` |
| 169 | idiomatic-rust | FunctionCall fields name and arguments are both Option but should be required... | `crates/llm-proxy-protocol/src/openai.rs:89` |
| 170 | api-design | ChatMessage.role is a bare String with no validation of known values | `crates/llm-proxy-protocol/src/openai.rs:116` |
| 171 | api-design | ChatCompletionRequest lacks deny_unknown_fields but uses flattened extra map | `crates/llm-proxy-protocol/src/openai.rs:159` |
| 172 | correctness | ChatCompletionRequest.max_tokens is i32 but should be non-negative | `crates/llm-proxy-protocol/src/openai.rs:175` |
| 173 | maintainability | stop field on ChatCompletionRequest duplicates deserialization logic from cor... | `crates/llm-proxy-protocol/src/openai.rs:190` |
| 174 | performance | UsageInfo computes total_tokens but providers also send it, causing potential... | `crates/llm-proxy-protocol/src/openai.rs:218` |
| 175 | api-design | Choice struct merges streaming delta and non-streaming message into one type | `crates/llm-proxy-protocol/src/openai.rs:237` |
| 176 | idiomatic-rust | ErrorResponse and ErrorDetails lack PartialEq despite all fields supporting e... | `crates/llm-proxy-protocol/src/openai.rs:313` |
| 177 | performance | Unnecessary allocation on the non-truncation fast path | `crates/llm-proxy-protocol/src/util.rs:17` |
| 178 | correctness | Silent panic on arithmetic underflow when suffix.len() > max_len | `crates/llm-proxy-protocol/src/util.rs:20` |
| 179 | test-coverage | Missing test for the documented panic when suffix.len() > max_len | `crates/llm-proxy-protocol/src/util.rs:30` |
| 180 | test-coverage | zen module has zero unit tests | `crates/llm-proxy-protocol/src/zen.rs:25` |
| 181 | correctness | GeminiRequest.stream is a field on the request body, not a query parameter | `crates/llm-proxy-protocol/src/zen.rs:173` |
| 182 | test-coverage | No integration test for CoreEvent serialization round-trip | `crates/llm-proxy-protocol/tests/core_exports.rs:1` |
| 183 | test-coverage | core_types_are_exported only declares variables; does not validate constructi... | `crates/llm-proxy-protocol/tests/core_exports.rs:16` |
| 184 | test-coverage | Serialization smoke test only asserts non-empty and a single field; does not ... | `crates/llm-proxy-protocol/tests/core_exports.rs:30` |
| 185 | correctness | build_core_response_from_output silently defaults to empty strings on missing... | `crates/llm-proxy-protocol/tests/fixture_tests.rs:108` |
| 186 | test-coverage | Redundant coverage-matrix tests: three separate tests check the same fixture ... | `crates/llm-proxy-protocol/tests/fixture_tests.rs:186` |
| 187 | idiomatic-rust | stop_sequence field is always set to None, not parsed from output.json | `crates/llm-proxy-protocol/tests/fixture_tests.rs:236` |
| 188 | test-coverage | Malformed case detection relies on exact directory name 'malformed' instead o... | `crates/llm-proxy-protocol/tests/fixture_tests.rs:490` |
| 189 | idiomatic-rust | Unnecessary allocation when encoding multi-byte characters in sanitize_tool_name | `crates/llm-proxy-provider/src/adapter/anthropic.rs:87` |
| 190 | correctness | Fallback to marking first tool block as closed when index not found | `crates/llm-proxy-provider/src/adapter/anthropic.rs:410` |
| 191 | error-handling | Tool block index fallback with unwrap_or_else(\|\| 0) could mask bugs | `crates/llm-proxy-provider/src/adapter/anthropic.rs:414` |
| 192 | correctness | message_stop fallback guesses stop_reason based on tool_blocks presence | `crates/llm-proxy-provider/src/adapter/anthropic.rs:459` |
| 193 | error-handling | unwrap_or in stop_sequences serialization silently produces Null on failure | `crates/llm-proxy-provider/src/adapter/anthropic.rs:712` |
| 194 | maintainability | ContentStart is emitted on first non-empty text delta but ContentStop is impl... | `crates/llm-proxy-provider/src/adapter/gemini.rs:94` |
| 195 | correctness | args_str.is_empty() check is unreachable for valid serde_json::Value serializ... | `crates/llm-proxy-provider/src/adapter/gemini.rs:153` |
| 196 | maintainability | finish() emits ToolCallStop for unclosed blocks but decode_frame already emit... | `crates/llm-proxy-provider/src/adapter/gemini.rs:208` |
| 197 | api-design | Multiple CoreRole variants silently map to 'user' in Gemini adapter | `crates/llm-proxy-provider/src/adapter/gemini.rs:263` |
| 198 | idiomatic-rust | Round-trip through serde_json for building GeminiContent is fragile and non-i... | `crates/llm-proxy-provider/src/adapter/gemini.rs:347` |
| 199 | api-design | tool_choice warning logs 'set' instead of the actual value, losing debuggability | `crates/llm-proxy-provider/src/adapter/gemini.rs:433` |
| 200 | api-design | ProviderStreamDecoder trait lacks Send bound on the trait object return | `crates/llm-proxy-provider/src/adapter/mod.rs:229` |
| 201 | api-design | protocol_names() returns an owned Vec instead of an iterator | `crates/llm-proxy-provider/src/adapter/mod.rs:272` |
| 202 | error-handling | Missing tool call index defaults to 0 | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:170` |
| 203 | correctness | Synthetic tool ID generated via uuid may collide with real IDs in multi-tool-... | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:186` |
| 204 | maintainability | finish() stop_reason heuristic uses tool_blocks which still contains entries ... | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:262` |
| 205 | correctness | Inconsistent schema validation: is_null() vs !is_object() | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:527` |
| 206 | api-design | stream_options only read from provider_hints.raw, not from a typed field on C... | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:591` |
| 207 | error-handling | decode_response uses SseFraming error variant for non-SSE error (missing mess... | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:628` |
| 208 | error-handling | Invalid JSON in tool_call.arguments silently replaced with empty object | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:662` |
| 209 | error-handling | Missing call_id on function_call output produces placeholder string instead o... | `crates/llm-proxy-provider/src/adapter/responses.rs:118` |
| 210 | correctness | Synthetic ToolCallStart emitted with empty id/name when output_item.added eve... | `crates/llm-proxy-provider/src/adapter/responses.rs:166` |
| 211 | performance | Unnecessary intermediate Vec allocation when joining system text parts | `crates/llm-proxy-provider/src/adapter/responses.rs:381` |
| 212 | idiomatic-rust | Wildcard match arm on CoreRole maps all future variants to 'user' | `crates/llm-proxy-provider/src/adapter/responses.rs:403` |
| 213 | maintainability | Redundant discovery config extraction in fetch_page duplicates the check in d... | `crates/llm-proxy-provider/src/discovery.rs:97` |
| 214 | error-handling | max_response_bytes limit uses bytes.len() + chunk.len() which can momentarily... | `crates/llm-proxy-provider/src/discovery.rs:126` |
| 215 | correctness | All discovered models unconditionally receive both ChatCompletions and Messag... | `crates/llm-proxy-provider/src/discovery.rs:186` |
| 216 | correctness | parse_models() hardcodes supports as [ChatCompletions, Messages] for all prov... | `crates/llm-proxy-provider/src/discovery.rs:193` |
| 217 | test-coverage | No test verifying supports field mapping for different provider kinds | `crates/llm-proxy-provider/src/discovery.rs:193` |
| 218 | correctness | Anthropic pagination silently returns None if has_more is true but last_id is... | `crates/llm-proxy-provider/src/discovery.rs:232` |
| 219 | error-handling | DiscoveryClient::default() uses expect() to construct HTTP client | `crates/llm-proxy-provider/src/discovery.rs:246` |
| 220 | performance | Regex replace_all produces intermediate Cow allocation per pattern | `crates/llm-proxy-provider/src/error.rs:164` |
| 221 | idiomatic-rust | Redundant wildcard arm in api_error_kind match | `crates/llm-proxy-provider/src/error.rs:205` |
| 222 | performance | O(n) buffer drain per line leads to O(n*m) total parsing cost | `crates/llm-proxy-provider/src/sse.rs:103` |
| 223 | correctness | Frame with only event or id but no data lines is emitted as a frame with empt... | `crates/llm-proxy-provider/src/sse.rs:177` |
| 224 | api-design | ProxyRequest fields are all pub with no builder or constructor | `crates/llm-proxy-provider/src/transport.rs:62` |
| 225 | error-handling | ProxyClient::new() uses expect() which can panic at runtime | `crates/llm-proxy-provider/src/transport.rs:129` |
| 226 | error-handling | ProxyClient::new() uses expect() with documented rationale | `crates/llm-proxy-provider/src/transport.rs:130` |
| 227 | performance | send_stream return type forces a heap allocation (Box + Pin) even for local use | `crates/llm-proxy-provider/src/transport.rs:188` |
| 228 | maintainability | Duplicated status-check logic between send_stream and check_status | `crates/llm-proxy-provider/src/transport.rs:205` |
| 229 | correctness | Silent truncation when max_tokens exceeds i32::MAX | `crates/llm-proxy-provider/tests/fixture_tests.rs:358` |
| 230 | performance | Unnecessary full-body serialization for model substring check | `crates/llm-proxy-provider/tests/fixture_tests.rs:502` |
| 231 | test-coverage | No negative test for encode_request failures | `crates/llm-proxy-provider/tests/fixture_tests.rs:965` |
| 232 | test-coverage | ContentKind match in ContentStart event comparison is incomplete | `crates/llm-proxy-provider/tests/fixture_tests.rs:1161` |
| 233 | correctness | Dead code: model field check block is a no-op | `crates/llm-proxy-provider/tests/llm-proxy-provider/tests/fixture_tests.rs:622` |
| 234 | performance | refresh_locks HashMap grows without bound | `crates/llm-proxy-server/src/catalog_service.rs:25` |
| 235 | correctness | catalog() clones catalog config on every call to check mode | `crates/llm-proxy-server/src/catalog_service.rs:54` |
| 236 | performance | Redundant config.clone() on every catalog() call | `crates/llm-proxy-server/src/catalog_service.rs:54` |
| 237 | test-coverage | cache_is_fresh has no unit test for TTL boundary behavior | `crates/llm-proxy-server/src/catalog_service.rs:55` |
| 238 | performance | load_disk_cache called on every catalog() invocation even when already populated | `crates/llm-proxy-server/src/catalog_service.rs:55` |
| 239 | correctness | Race between cache_is_fresh check and refresh | `crates/llm-proxy-server/src/catalog_service.rs:58` |
| 240 | performance | Unnecessary models.clone() inside read-locked section | `crates/llm-proxy-server/src/catalog_service.rs:65` |
| 241 | correctness | cache_is_fresh clones the entire catalog config on every call to read TTL | `crates/llm-proxy-server/src/catalog_service.rs:168` |
| 242 | performance | cache_is_fresh clones provider.catalog unnecessarily | `crates/llm-proxy-server/src/catalog_service.rs:168` |
| 243 | correctness | cache_is_fresh uses signed integer comparison that silently accepts future ti... | `crates/llm-proxy-server/src/catalog_service.rs:181` |
| 244 | performance | Full file read into string for TOML parsing with no size limit | `crates/llm-proxy-server/src/catalog_service.rs:205` |
| 245 | maintainability | write_catalog_atomic is overly conservative with symlink rejection | `crates/llm-proxy-server/src/catalog_service.rs:240` |
| 246 | test-coverage | No test for concurrent successful refresh | `crates/llm-proxy-server/src/catalog_service.rs:375` |
| 247 | api-design | is_duplicate() ignores the path, making it easy to misuse | `crates/llm-proxy-server/src/middleware.rs:65` |
| 248 | correctness | Deduplicator never removes entries for in-flight requests, only expired ones | `crates/llm-proxy-server/src/middleware.rs:87` |
| 249 | performance | Pruning runs on every single request | `crates/llm-proxy-server/src/middleware.rs:87` |
| 250 | performance | SHA-256 is unnecessarily heavy for request deduplication | `crates/llm-proxy-server/src/middleware.rs:98` |
| 251 | maintainability | RateLimiter and RequestDeduplicator have similar but independently maintained... | `crates/llm-proxy-server/src/middleware.rs:157` |
| 252 | security | RateLimiter accepts u32::MAX as max_requests_per_minute with no validation | `crates/llm-proxy-server/src/middleware.rs:169` |
| 253 | maintainability | Magic number 300 for rate limiter eviction threshold | `crates/llm-proxy-server/src/middleware.rs:194` |
| 254 | idiomatic-rust | RequestIdGenerator silently falls back to unix timestamp 0 on clock error | `crates/llm-proxy-server/src/middleware.rs:234` |
| 255 | idiomatic-rust | RequestIdGenerator uses Relaxed ordering for a counter that only needs monoto... | `crates/llm-proxy-server/src/middleware.rs:238` |
| 256 | security | X-Forwarded-For takes the leftmost (first) IP unconditionally | `crates/llm-proxy-server/src/middleware.rs:281` |
| 257 | test-coverage | No test for multi-value X-Forwarded-For header | `crates/llm-proxy-server/src/middleware.rs:283` |
| 258 | security | X-Real-Ip header is also trusted without validation | `crates/llm-proxy-server/src/middleware.rs:295` |
| 259 | test-coverage | No test for get_client_ip returning 'unknown' when no ConnectInfo is available | `crates/llm-proxy-server/src/middleware.rs:306` |
| 260 | idiomatic-rust | get_client_ip returns string "unknown" when no IP is available | `crates/llm-proxy-server/src/middleware.rs:310` |
| 261 | maintainability | No CORS headers are set on the response | `crates/llm-proxy-server/src/routes/chat.rs:1` |
| 262 | security | Provider name from URL path is interpolated into a format string without vali... | `crates/llm-proxy-server/src/routes/chat.rs:57` |
| 263 | correctness | Body is deserialized twice: once by prepare_request (for dedup hash) and once... | `crates/llm-proxy-server/src/routes/chat.rs:67` |
| 264 | test-coverage | No unit or integration tests for the chat completions handler | `crates/llm-proxy-server/src/routes/chat.rs:86` |
| 265 | correctness | OpenAI `stream_options.include_usage` extraction silently ignores non-boolean... | `crates/llm-proxy-server/src/routes/core_pipeline.rs:74` |
| 266 | performance | Streaming output uses unbounded oneshot/channel pattern that may buffer event... | `crates/llm-proxy-server/src/routes/core_pipeline.rs:81` |
| 267 | idiomatic-rust | Silent u64-to-i64 narrowing cast on `created` timestamp | `crates/llm-proxy-server/src/routes/core_pipeline.rs:87` |
| 268 | maintainability | `wrap_anthropic_events` clones `r#type` from the event before serialization, ... | `crates/llm-proxy-server/src/routes/core_pipeline.rs:146` |
| 269 | security | Loopback bypass for rate limiting trusts socket address without proxy header ... | `crates/llm-proxy-server/src/routes/core_pipeline.rs:229` |
| 270 | security | Loopback bypass for rate limiting can be exploited in production | `crates/llm-proxy-server/src/routes/core_pipeline.rs:230` |
| 271 | maintainability | validate_and_clean_provider_name does not clean anything despite its name | `crates/llm-proxy-server/src/routes/core_pipeline.rs:278` |
| 272 | idiomatic-rust | `validate_and_clean_provider_name` does not actually clean anything | `crates/llm-proxy-server/src/routes/core_pipeline.rs:278` |
| 273 | performance | Double provider registry lookup when catalog enforcement is enabled | `crates/llm-proxy-server/src/routes/core_pipeline.rs:305` |
| 274 | performance | Double provider lookup in catalog enforcement path | `crates/llm-proxy-server/src/routes/core_pipeline.rs:312` |
| 275 | correctness | catalog_contains_model special-cases Gemini with strip_prefix but not other p... | `crates/llm-proxy-server/src/routes/core_pipeline.rs:379` |
| 276 | maintainability | catalog_contains_model has hardcoded Gemini protocol string | `crates/llm-proxy-server/src/routes/core_pipeline.rs:379` |
| 277 | maintainability | Gemini-specific model matching logic hardcoded in `catalog_contains_model` | `crates/llm-proxy-server/src/routes/core_pipeline.rs:379` |
| 278 | maintainability | HEARTBEAT_INTERVAL is a hardcoded magic constant | `crates/llm-proxy-server/src/routes/core_pipeline.rs:512` |
| 279 | test-coverage | HEARTBEAT_INTERVAL is hardcoded and not tested at boundary values | `crates/llm-proxy-server/src/routes/core_pipeline.rs:513` |
| 280 | correctness | Stream encoder uses synthetic message ID instead of upstream provider's real ID | `crates/llm-proxy-server/src/routes/core_pipeline.rs:614` |
| 281 | maintainability | Spawned streaming task has no name for debugging | `crates/llm-proxy-server/src/routes/core_pipeline.rs:813` |
| 282 | error-handling | `encode_core_event` silently drops encoding errors with only a warn log | `crates/llm-proxy-server/src/routes/core_pipeline.rs:1084` |
| 283 | correctness | OpenAI error_type override shadows the shared extract_error_fields value sile... | `crates/llm-proxy-server/src/routes/error_response.rs:227` |
| 284 | maintainability | openai_stream_error_json is marked #[allow(dead_code)] | `crates/llm-proxy-server/src/routes/error_response.rs:264` |
| 285 | maintainability | openai_stream_error_json is dead code with a suppression attribute | `crates/llm-proxy-server/src/routes/error_response.rs:264` |
| 286 | security | UpstreamTimeout message is silently discarded but not consistently sanitized ... | `crates/llm-proxy-server/src/routes/error_response.rs:311` |
| 287 | api-design | map_upstream_status silently swallows all 1xx/2xx/3xx upstream codes as 502 | `crates/llm-proxy-server/src/routes/error_response.rs:367` |
| 288 | test-coverage | No tests for openai_stream_error_json_with_type output structure | `crates/llm-proxy-server/src/routes/error_response.rs:397` |
| 289 | maintainability | HealthMetrics struct is a manual field-by-field copy of Snapshot that will si... | `crates/llm-proxy-server/src/routes/health.rs:13` |
| 290 | correctness | Metrics counter type is i64 but can never be negative -- u64 would be semanti... | `crates/llm-proxy-server/src/routes/health.rs:24` |
| 291 | test-coverage | No unit tests for the health handler -- tests exist only at integration level | `crates/llm-proxy-server/src/routes/health.rs:37` |
| 292 | security | Version endpoint exposes build target and git SHA without authentication | `crates/llm-proxy-server/src/routes/health.rs:76` |
| 293 | maintainability | VersionBody.name duplicates BuildInfo.name but uses server_name() instead | `crates/llm-proxy-server/src/routes/health.rs:87` |
| 294 | correctness | Double deserialization of request body after pre-flight hash | `crates/llm-proxy-server/src/routes/messages.rs:64` |
| 295 | maintainability | Redundant validate() call before decode_request that also validates | `crates/llm-proxy-server/src/routes/messages.rs:70` |
| 296 | maintainability | Source guard test uses include_str! with fragile split_once heuristic | `crates/llm-proxy-server/src/routes/messages.rs:117` |
| 297 | maintainability | Magic constant MAX_BODY_BYTES with no configuration option | `crates/llm-proxy-server/src/routes/mod.rs:27` |
| 298 | api-design | serde(deny_unknown_fields) on response serialization types has no effect | `crates/llm-proxy-server/src/routes/models.rs:28` |
| 299 | security | ModelsQuery accepts arbitrary refresh parameter without validation | `crates/llm-proxy-server/src/routes/models.rs:70` |
| 300 | idiomatic-rust | ModelsQuery.refresh is typed as Option<String> but only accepts exact value "... | `crates/llm-proxy-server/src/routes/models.rs:70` |
| 301 | api-design | Models endpoint uses OpenAI error shape for GET requests but Anthropic path c... | `crates/llm-proxy-server/src/routes/models.rs:88` |
| 302 | security | No rate limiting or access control on refresh=live parameter | `crates/llm-proxy-server/src/routes/models.rs:88` |
| 303 | api-design | Models endpoint always uses ClientProtocol::OpenAiChat for error encoding reg... | `crates/llm-proxy-server/src/routes/models.rs:93` |
| 304 | idiomatic-rust | String allocation per supports entry in hot map closure | `crates/llm-proxy-server/src/routes/models.rs:127` |
| 305 | maintainability | Pagination fields first_id/last_id clone model IDs that are already owned by ... | `crates/llm-proxy-server/src/routes/models.rs:141` |
| 306 | idiomatic-rust | Redundant Content-Type header insertion after axum::Json already sets it | `crates/llm-proxy-server/src/routes/models.rs:152` |
| 307 | error-handling | unwrap_or in map_catalog_error may mask invalid status codes | `crates/llm-proxy-server/src/routes/models.rs:164` |
| 308 | test-coverage | No integration test for the full handle_models Axum handler | `crates/llm-proxy-server/src/routes/models.rs:355` |
| 309 | correctness | Non-text CoreContent variants silently produce empty strings in per-message t... | `crates/llm-proxy-server/src/routes/token_count.rs:49` |
| 310 | idiomatic-rust | Redundant provider existence check in count_tokens_inner | `crates/llm-proxy-server/src/routes/token_count.rs:59` |
| 311 | maintainability | Hardcoded request path string duplicates routing configuration | `crates/llm-proxy-server/src/routes/token_count.rs:62` |
| 312 | correctness | Deserialization of untrusted JSON does not guard against deeply nested or adv... | `crates/llm-proxy-server/src/routes/token_count.rs:65` |
| 313 | idiomatic-rust | Full request decode is performed when only text extraction is needed | `crates/llm-proxy-server/src/routes/token_count.rs:73` |
| 314 | performance | Unbounded String allocation from concatenated system text blocks | `crates/llm-proxy-server/src/routes/token_count.rs:88` |
| 315 | idiomatic-rust | Intermediate String allocation per message for text extraction | `crates/llm-proxy-server/src/routes/token_count.rs:104` |
| 316 | test-coverage | No unit tests for the shutdown module | `crates/llm-proxy-server/src/shutdown.rs:4` |
| 317 | error-handling | shutdown_signal uses expect() which panics if Ctrl-C handler installation fails | `crates/llm-proxy-server/src/shutdown.rs:8` |
| 318 | maintainability | Shutdown signal is consumed only once, so repeated Ctrl-C kills the process hard | `crates/llm-proxy-server/src/shutdown.rs:22` |
| 319 | api-design | `new_with_catalog_dir` parameter count is high (6 positional params) making c... | `crates/llm-proxy-server/src/state.rs:127` |
| 320 | correctness | Duration::as_millis() to u64 conversion saturates to u64::MAX instead of clam... | `crates/llm-proxy-server/src/state.rs:137` |
| 321 | api-design | Unused parameter _model_name in state_with_provider | `crates/llm-proxy-server/tests/chat_completions.rs:92` |
| 322 | maintainability | Hardcoded config values repeated in three places | `crates/llm-proxy-server/tests/chat_completions.rs:135` |
| 323 | maintainability | Near-duplicate test: non_streaming_text_request_returns_openai_shaped_respons... | `crates/llm-proxy-server/tests/chat_completions.rs:377` |
| 324 | maintainability | Magic number 64 * 1024 and 1024 * 1024 repeated 29 times | `crates/llm-proxy-server/tests/chat_completions.rs:387` |
| 325 | test-coverage | tool_choice assertion only validates object form, not string passthrough | `crates/llm-proxy-server/tests/chat_completions.rs:555` |
| 326 | maintainability | Function name says '400' but test asserts 404 | `crates/llm-proxy-server/tests/chat_completions.rs:578` |
| 327 | test-coverage | No tests for HTTP methods other than POST on chat completions route | `crates/llm-proxy-server/tests/chat_completions.rs:700` |
| 328 | correctness | Weak assertion on tool_call delta arguments: always passes | `crates/llm-proxy-server/tests/chat_completions.rs:905` |
| 329 | security | Source guard test uses string-matching on production code which is fragile | `crates/llm-proxy-server/tests/chat_completions.rs:1080` |
| 330 | maintainability | Triplicate coverage: three tests exercise empty-state 404 with OpenAI error s... | `crates/llm-proxy-server/tests/chat_completions.rs:1525` |
| 331 | idiomatic-rust | Unused parameter _model_name in state_with_provider | `crates/llm-proxy-server/tests/core_pipeline.rs:124` |
| 332 | correctness | Fallback route config silently assigns messages route to unknown protocols | `crates/llm-proxy-server/tests/core_pipeline.rs:135` |
| 333 | maintainability | Hard-coded URL path /providers/mock-provider/v1/messages in mock URL | `crates/llm-proxy-server/tests/core_pipeline.rs:246` |
| 334 | maintainability | Repeated magic number 64 * 1024 for body read limit | `crates/llm-proxy-server/tests/core_pipeline.rs:586` |
| 335 | maintainability | Hard-coded sleep for request tracker verification is fragile | `crates/llm-proxy-server/tests/core_pipeline.rs:656` |
| 336 | test-coverage | No test for unknown provider name in URL path | `crates/llm-proxy-server/tests/core_pipeline.rs:730` |
| 337 | security | No test for provider name validation in URL path | `crates/llm-proxy-server/tests/core_pipeline.rs:730` |
| 338 | test-coverage | No test for HTTP method mismatch (GET on POST-only route) | `crates/llm-proxy-server/tests/core_pipeline.rs:730` |
| 339 | test-coverage | No test for missing or wrong content-type header | `crates/llm-proxy-server/tests/core_pipeline.rs:730` |
| 340 | test-coverage | No test for request timeout behavior | `crates/llm-proxy-server/tests/core_pipeline.rs:730` |
| 341 | performance | Streaming tests read entire SSE body into memory with 1 MiB limit | `crates/llm-proxy-server/tests/core_pipeline.rs:795` |
| 342 | test-coverage | Duplicate test: stream_error_after_first_byte and malformed_stream_frame_is_h... | `crates/llm-proxy-server/tests/core_pipeline.rs:996` |
| 343 | test-coverage | No test for invalid JSON on streaming requests | `crates/llm-proxy-server/tests/core_pipeline.rs:1367` |
| 344 | maintainability | Hard-coded sleep for body capture verification is fragile | `crates/llm-proxy-server/tests/core_pipeline.rs:1429` |
| 345 | test-coverage | No integration tests for Anthropic streaming through /v1/messages route | `crates/llm-proxy-server/tests/integration.rs:1` |
| 346 | test-coverage | No integration test for provider name validation on messages and count_tokens... | `crates/llm-proxy-server/tests/integration.rs:1` |
| 347 | test-coverage | No test for count_tokens with invalid JSON or missing fields | `crates/llm-proxy-server/tests/integration.rs:1` |
| 348 | test-coverage | No test for HTTP method mismatch (e.g. GET on POST-only routes) | `crates/llm-proxy-server/tests/integration.rs:1` |
| 349 | maintainability | Duplicated AppState construction boilerplate across test helpers | `crates/llm-proxy-server/tests/integration.rs:21` |
| 350 | test-coverage | health_returns_ok_with_body asserts model_counts is absent but does not check... | `crates/llm-proxy-server/tests/integration.rs:105` |
| 351 | test-coverage | messages_invalid_json_returns_bad_request uses empty-provider state for an er... | `crates/llm-proxy-server/tests/integration.rs:253` |
| 352 | correctness | count_tokens_returns_estimate test does not assert the response shape | `crates/llm-proxy-server/tests/integration.rs:322` |
| 353 | performance | messages_oversized_body_returns_payload_too_large allocates 33 MiB in test | `crates/llm-proxy-server/tests/integration.rs:469` |
| 354 | maintainability | TOML-mode tests duplicate assertions from earlier tests with minimal additions | `crates/llm-proxy-server/tests/integration.rs:487` |
| 355 | api-design | max_tokens is i32 but the Anthropic API uses unsigned integers | `crates/llm-proxy/crates/llm-proxy-protocol/src/anthropic.rs:22` |

</details>

---

## ⚪ INFO Findings (Compact)

167 informational findings. Top themes:

- **maintainability** (41 findings)
- **test-coverage** (29 findings)
- **api-design** (27 findings)
- **correctness** (23 findings)
- **performance** (18 findings)
- **security** (15 findings)
- **idiomatic-rust** (11 findings)
- **error-handling** (3 findings)

<details>
<summary>View all INFO findings</summary>

| # | Category | Title | File:Line |
|---|----------|-------|-----------|
| 1 | maintainability | TARGET env var set after Emitter::emit() -- ordering dependency in build script | `apps/llm-proxy/build.rs:14` |
| 2 | security | Default provider configs reference API keys via environment variable interpol... | `apps/llm-proxy/src/main.rs:148` |
| 3 | correctness | PID file written before config is loaded -- stale PID file left on config error | `apps/llm-proxy/src/main.rs:496` |
| 4 | security | format_desktop_entry escaping is incomplete for XDG Exec keys | `apps/llm-proxy/src/main.rs:1197` |
| 5 | maintainability | llm-proxy-api stub crate is published as a workspace member with zero code | `crates/llm-proxy-api/src/lib.rs:1` |
| 6 | maintainability | llm-proxy-api stub crate is intentionally empty -- no findings | `crates/llm-proxy-api/src/lib.rs:1` |
| 7 | maintainability | API stub crate is an empty placeholder | `crates/llm-proxy-api/src/lib.rs:1` |
| 8 | maintainability | regex crate used where glob crate or globset would be more idiomatic | `crates/llm-proxy-core/src/catalog.rs:5` |
| 9 | api-design | CatalogFile is a thin wrapper that adds nesting but no value | `crates/llm-proxy-core/src/catalog.rs:10` |
| 10 | maintainability | glob_matches function lacks documentation | `crates/llm-proxy-core/src/catalog.rs:80` |
| 11 | maintainability | Regex dependency used for a simple pattern that could be parsed without it | `crates/llm-proxy-core/src/env_interpolate.rs:6` |
| 12 | maintainability | Module-level docs are good but individual public functions lack # Example sec... | `crates/llm-proxy-core/src/env_interpolate.rs:36` |
| 13 | security | Interpolated values are not validated or sanitized before being used as confi... | `crates/llm-proxy-core/src/env_interpolate.rs:36` |
| 14 | correctness | find_unresolved_env_var name is misleading -- it finds patterns, not unresolv... | `crates/llm-proxy-core/src/env_interpolate.rs:48` |
| 15 | test-coverage | No test for adjacent patterns like ${A}${B} or nested-looking patterns | `crates/llm-proxy-core/src/env_interpolate.rs:73` |
| 16 | api-design | CoreError has no variant for PID-related errors | `crates/llm-proxy-core/src/error.rs:8` |
| 17 | maintainability | ProviderResolution variant duplicates the unstructured-message pattern from C... | `crates/llm-proxy-core/src/error.rs:29` |
| 18 | maintainability | Metrics struct fields are all private with no accessor methods | `crates/llm-proxy-core/src/metrics.rs:42` |
| 19 | api-design | Ordering::Relaxed used for all atomic operations | `crates/llm-proxy-core/src/metrics.rs:63` |
| 20 | maintainability | Redundant word in doc comment: 'deduplicated (deduplicated)' | `crates/llm-proxy-core/src/metrics.rs:110` |
| 21 | api-design | Snapshot does not derive Default or PartialEq | `crates/llm-proxy-core/src/metrics.rs:166` |
| 22 | maintainability | percentile doc comment says '0..=100' but function does not enforce this | `crates/llm-proxy-core/src/metrics.rs:208` |
| 23 | correctness | percentile() does not validate the pct parameter range | `crates/llm-proxy-core/src/metrics.rs:214` |
| 24 | test-coverage | Tests for Snapshot manually construct instances with many zero fields instead... | `crates/llm-proxy-core/src/metrics.rs:353` |
| 25 | idiomatic-rust | u32 type for PID is non-standard; libc::pid_t (i32) is the correct POSIX type | `crates/llm-proxy-core/src/pid.rs:51` |
| 26 | api-design | is_process_running is a free function disguised as an associated method | `crates/llm-proxy-core/src/pid.rs:88` |
| 27 | maintainability | Duplication between pid.rs library code and main.rs binary code | `crates/llm-proxy-core/src/pid.rs:88` |
| 28 | api-design | ProviderCatalogConfig implements both manual Default and serde defaults, risk... | `crates/llm-proxy-core/src/provider_config.rs:39` |
| 29 | api-design | ServerConfig.log_level is an unvalidated String accepting arbitrary values | `crates/llm-proxy-core/src/provider_config.rs:46` |
| 30 | maintainability | dedup_window uses Duration::ZERO to mean disabled, not clearly represented | `crates/llm-proxy-core/src/provider_config.rs:57` |
| 31 | api-design | ProviderFile is a thin wrapper that adds noise to the public API | `crates/llm-proxy-core/src/provider_config.rs:83` |
| 32 | maintainability | Discovery default functions are const but not the cache_ttl default | `crates/llm-proxy-core/src/provider_config.rs:307` |
| 33 | test-coverage | No unit tests for validate_provider_config happy path or several error variants | `crates/llm-proxy-core/src/provider_config.rs:620` |
| 34 | maintainability | Endpoint scheme validation duplicated between adapter and discovery sections | `crates/llm-proxy-core/src/provider_config.rs:708` |
| 35 | correctness | known_protocols linear scan on every adapter | `crates/llm-proxy-core/src/provider_config.rs:720` |
| 36 | performance | known_protocols linear scan on every adapter validation | `crates/llm-proxy-core/src/provider_config.rs:721` |
| 37 | security | Discovery endpoint validation uses "discovery" as adapter name in errors, whi... | `crates/llm-proxy-core/src/provider_config.rs:762` |
| 38 | test-coverage | No test for unresolved env var in provider config loading | `crates/llm-proxy-core/src/provider_config.rs:1066` |
| 39 | test-coverage | No test for discovery header CRLF validation | `crates/llm-proxy-core/src/provider_config.rs:1066` |
| 40 | test-coverage | No test for empty adapter protocol, empty adapter name, or empty model alias ... | `crates/llm-proxy-core/src/provider_config.rs:1066` |
| 41 | api-design | ProviderAdapterTargetConfig derives PartialEq but not Eq | `crates/llm-proxy-core/src/provider_registry.rs:78` |
| 42 | performance | load_from_dir reads entire directory into memory before filtering | `crates/llm-proxy-core/src/provider_registry.rs:159` |
| 43 | maintainability | load_provider_config called with None for known_protocols, deferred validation | `crates/llm-proxy-core/src/provider_registry.rs:216` |
| 44 | performance | validate_protocols allocates HashSet<String> even though caller typically pas... | `crates/llm-proxy-core/src/provider_registry.rs:286` |
| 45 | correctness | Model alias lookup does not normalize case or whitespace | `crates/llm-proxy-core/src/provider_registry.rs:378` |
| 46 | api-design | ProviderRegistry has len() and is_empty() but no IntoIterator or Iterator imp... | `crates/llm-proxy-core/src/provider_registry.rs:399` |
| 47 | api-design | ProviderRegistry::iter returns an opaque impl Iterator, limiting downstream f... | `crates/llm-proxy-core/src/provider_registry.rs:416` |
| 48 | api-design | iter() returns an opaque iterator, not implementing IntoIterator for &Provide... | `crates/llm-proxy-core/src/provider_registry.rs:416` |
| 49 | maintainability | Module doc comment references provider_config but module is only used from en... | `crates/llm-proxy-core/src/test_support.rs:3` |
| 50 | idiomatic-rust | TEST_ENV_LOCK uses std::sync::Mutex instead of parking_lot::Mutex | `crates/llm-proxy-core/src/test_support.rs:22` |
| 51 | test-coverage | test_support module itself has no unit tests | `crates/llm-proxy-core/src/test_support.rs:22` |
| 52 | maintainability | Poison recovery silently masks test failures | `crates/llm-proxy-core/src/test_support.rs:36` |
| 53 | idiomatic-rust | MessageContent derives PartialEq and Eq but is never compared in production code | `crates/llm-proxy-core/src/token/counter.rs:8` |
| 54 | idiomatic-rust | MessageContent is public but lacks Hash derive | `crates/llm-proxy-core/src/token/counter.rs:8` |
| 55 | api-design | MessageContent could benefit from serde derives for deserialization | `crates/llm-proxy-core/src/token/counter.rs:8` |
| 56 | api-design | MessageContent lacks serde derives despite being constructed from deserialize... | `crates/llm-proxy-core/src/token/counter.rs:9` |
| 57 | api-design | MessageContent is a plain struct but could be a newtype or use borrowed strings | `crates/llm-proxy-core/src/token/counter.rs:9` |
| 58 | api-design | Counter is a zero-sized type with no state, new() is unnecessary | `crates/llm-proxy-core/src/token/counter.rs:32` |
| 59 | performance | Counter is a zero-sized unit struct wrapped in Arc unnecessarily | `crates/llm-proxy-core/src/token/counter.rs:32` |
| 60 | idiomatic-rust | Counter methods take &self but Counter has no fields | `crates/llm-proxy-core/src/token/counter.rs:34` |
| 61 | correctness | count_tokens silently ignores non-text semantics (whitespace-only strings cou... | `crates/llm-proxy-core/src/token/counter.rs:44` |
| 62 | correctness | count_messages applies SYSTEM_OVERHEAD even when system prompt is empty | `crates/llm-proxy-core/src/token/counter.rs:65` |
| 63 | test-coverage | No test for very large input strings | `crates/llm-proxy-core/src/token/counter.rs:65` |
| 64 | test-coverage | No test for multi-byte Unicode input in count_tokens | `crates/llm-proxy-core/src/token/counter.rs:85` |
| 65 | test-coverage | No test for whitespace-only input | `crates/llm-proxy-core/src/token/counter.rs:85` |
| 66 | api-design | mod.rs re-exports MessageContent which is only used as an internal intermedia... | `crates/llm-proxy-core/src/token/mod.rs:9` |
| 67 | maintainability | Deprecated output field has no #[deprecated] attribute | `crates/llm-proxy-protocol/src/anthropic.rs:285` |
| 68 | performance | Unknown block type serialization clones all 14 fields | `crates/llm-proxy-protocol/src/anthropic.rs:557` |
| 69 | maintainability | MessageResponse derives Deserialize despite being outbound-only | `crates/llm-proxy-protocol/src/anthropic.rs:641` |
| 70 | maintainability | decode_request always creates empty provider_hints | `crates/llm-proxy-protocol/src/client/anthropic.rs:90` |
| 71 | api-design | ProtocolError has no source chain for underlying errors | `crates/llm-proxy-protocol/src/client/mod.rs:39` |
| 72 | performance | String allocation for static string literals in encode_finish_reason | `crates/llm-proxy-protocol/src/client/openai_chat.rs:43` |
| 73 | api-design | decode_tool_choice is a private free function, not a method on any type | `crates/llm-proxy-protocol/src/client/openai_chat.rs:281` |
| 74 | performance | StreamEncoder clones id and model strings on every make_chunk call | `crates/llm-proxy-protocol/src/client/openai_chat.rs:537` |
| 75 | correctness | ToolCallStart index is clamped to i32::MAX which could map different indices ... | `crates/llm-proxy-protocol/src/client/openai_chat.rs:666` |
| 76 | maintainability | RedactedThinking.data is serde_json::Value but semantically should be a String | `crates/llm-proxy-protocol/src/core.rs:281` |
| 77 | api-design | ChatCompletionResponse does not capture service_tier, system_fingerprint, or ... | `crates/llm-proxy-protocol/src/openai.rs:264` |
| 78 | api-design | Module-level doc comment claims 'used across multiple crates' but it is a sma... | `crates/llm-proxy-protocol/src/util.rs:16` |
| 79 | correctness | UTF-8 boundary loop produces output shorter than max_len when content ends mi... | `crates/llm-proxy-protocol/src/util.rs:22` |
| 80 | maintainability | GeminiUsage uses serde rename_all = 'camelCase' which differs from other Gemi... | `crates/llm-proxy-protocol/src/zen.rs:319` |
| 81 | maintainability | Duplicate streaming case lists appear in three places | `crates/llm-proxy-protocol/tests/fixture_tests.rs:108` |
| 82 | correctness | i64 to i32 cast for token counts may silently truncate large values | `crates/llm-proxy-protocol/tests/fixture_tests.rs:241` |
| 83 | test-coverage | OpenAI StreamEncoder::new hard-codes created=1000 and include_usage=false | `crates/llm-proxy-protocol/tests/fixture_tests.rs:1031` |
| 84 | security | Tool name sanitization uses collision-safe reversible encoding with sentinel ... | `crates/llm-proxy-provider/src/adapter/anthropic.rs:1` |
| 85 | test-coverage | Good test coverage across encode, decode, streaming, and edge cases | `crates/llm-proxy-provider/src/adapter/anthropic.rs:554` |
| 86 | api-design | new_stream_decoder returns Box<dyn + Send> but not Sync | `crates/llm-proxy-provider/src/adapter/anthropic.rs:840` |
| 87 | performance | build_tool_name_map() provides O(1) tool name lookups | `crates/llm-proxy-provider/src/adapter/gemini.rs:1` |
| 88 | idiomatic-rust | unwrap_or_else with closure that only formats a string is equivalent to unwra... | `crates/llm-proxy-provider/src/adapter/gemini.rs:149` |
| 89 | correctness | CoreRole::System mapped to 'user' may produce invalid Gemini conversations | `crates/llm-proxy-provider/src/adapter/gemini.rs:265` |
| 90 | correctness | Non-object input_schema silently replaced with empty object, losing schema in... | `crates/llm-proxy-provider/src/adapter/gemini.rs:401` |
| 91 | test-coverage | Test asserts fallback name is '0' for tool_use_id 'gemini_call_0' -- validate... | `crates/llm-proxy-provider/src/adapter/gemini.rs:1037` |
| 92 | security | Debug impl for ProviderAdapterTarget properly redacts API key and query param... | `crates/llm-proxy-provider/src/adapter/mod.rs:114` |
| 93 | api-design | ProviderAdapter uses enum dispatch pattern for zero-cost variant selection | `crates/llm-proxy-provider/src/adapter/mod.rs:145` |
| 94 | correctness | provider_hints warning fires on every request when hints are present | `crates/llm-proxy-provider/src/adapter/mod.rs:179` |
| 95 | api-design | ProviderStreamDecoder trait with decode_frame/finish lifecycle | `crates/llm-proxy-provider/src/adapter/mod.rs:229` |
| 96 | security | expand_url_template() validates model names against path traversal | `crates/llm-proxy-provider/src/adapter/mod.rs:309` |
| 97 | performance | expand_url_template scans template three times for {model} | `crates/llm-proxy-provider/src/adapter/mod.rs:313` |
| 98 | security | expand_url_template validates model names against path traversal | `crates/llm-proxy-provider/src/adapter/mod.rs:321` |
| 99 | security | is_safe_model_component uses byte-level validation covering ASCII range | `crates/llm-proxy-provider/src/adapter/mod.rs:347` |
| 100 | test-coverage | Comprehensive test suite covering protocols, URL safety, usage mapping, and s... | `crates/llm-proxy-provider/src/adapter/mod.rs:460` |
| 101 | performance | close_tool_blocks collects and sorts indices on every call regardless of map ... | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:307` |
| 102 | idiomatic-rust | Repeated ChatMessage construction could use a builder or Default derivation | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:339` |
| 103 | error-handling | serde_json::to_string failure on ToolUse input silently replaced with empty J... | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:421` |
| 104 | test-coverage | No test for concurrent text + reasoning + tool_call interleaving in a single ... | `crates/llm-proxy-provider/src/adapter/openai_chat.rs:748` |
| 105 | test-coverage | No test for encoding a message with mixed text and tool use content | `crates/llm-proxy-provider/src/adapter/responses.rs:699` |
| 106 | test-coverage | No test for response.failed with error code mapping to Authentication or Inva... | `crates/llm-proxy-provider/src/adapter/responses.rs:699` |
| 107 | test-coverage | No test for response.output_text.done event resetting content state | `crates/llm-proxy-provider/src/adapter/responses.rs:699` |
| 108 | maintainability | Anthropic version header constant duplicated from adapter layer | `crates/llm-proxy-provider/src/discovery.rs:14` |
| 109 | correctness | dedup_by only removes consecutive duplicates after sort_by on id | `crates/llm-proxy-provider/src/discovery.rs:80` |
| 110 | correctness | HTTP status checked after body is fully consumed | `crates/llm-proxy-provider/src/discovery.rs:134` |
| 111 | performance | Unnecessary Vec allocation for supports on every model entry | `crates/llm-proxy-provider/src/discovery.rs:196` |
| 112 | performance | set_query_parameter allocates Vec to rebuild query pairs on every pagination ... | `crates/llm-proxy-provider/src/discovery.rs:207` |
| 113 | test-coverage | Test sorts models a second time, masking whether parse_models produces sorted... | `crates/llm-proxy-provider/src/discovery.rs:424` |
| 114 | test-coverage | No test for FireworksAccountModels pagination | `crates/llm-proxy-provider/src/discovery.rs:443` |
| 115 | test-coverage | Missing unit test for Anthropic pagination next_page logic | `crates/llm-proxy-provider/src/discovery.rs:449` |
| 116 | api-design | ProviderError is non-exhaustive with convenience constructor | `crates/llm-proxy-provider/src/error.rs:14` |
| 117 | maintainability | SseFraming and EmptyResponse variants store String but lack structured context | `crates/llm-proxy-provider/src/error.rs:55` |
| 118 | security | From<reqwest::Error> strips URLs to prevent credential leakage | `crates/llm-proxy-provider/src/error.rs:66` |
| 119 | correctness | From<reqwest::Error> impl uses without_url() which may lose useful non-sensit... | `crates/llm-proxy-provider/src/error.rs:66` |
| 120 | idiomatic-rust | Redaction regex patterns use expect() in LazyLock initialization | `crates/llm-proxy-provider/src/error.rs:129` |
| 121 | security | sanitize_api_error_body runs redaction before truncation with documented rati... | `crates/llm-proxy-provider/src/error.rs:161` |
| 122 | test-coverage | Redaction pattern tests cover all 12 regex patterns with positive and negativ... | `crates/llm-proxy-provider/src/error.rs:222` |
| 123 | test-coverage | Crate enforces #![deny(missing_docs)] for documentation completeness | `crates/llm-proxy-provider/src/lib.rs:6` |
| 124 | idiomatic-rust | Unnecessary .into_iter().collect() for Option<SseFrame> | `crates/llm-proxy-provider/src/sse.rs:83` |
| 125 | correctness | Only one optional space after colon is stripped, per spec | `crates/llm-proxy-provider/src/sse.rs:140` |
| 126 | test-coverage | Missing test for field with no colon and no value (e.g., bare 'data' line) | `crates/llm-proxy-provider/src/sse.rs:306` |
| 127 | maintainability | Source guard test couples test code to production source file content | `crates/llm-proxy-provider/src/sse.rs:566` |
| 128 | security | AuthHeaders Debug impl correctly redacts api_key | `crates/llm-proxy-provider/src/transport.rs:44` |
| 129 | security | ProxyRequest Debug impl redacts api_key and URL query parameters | `crates/llm-proxy-provider/src/transport.rs:84` |
| 130 | maintainability | endpoint_without_query is a private free function that could be a method | `crates/llm-proxy-provider/src/transport.rs:94` |
| 131 | maintainability | Magic numbers for connection pool tuning | `crates/llm-proxy-provider/src/transport.rs:120` |
| 132 | performance | ProxyClient uses connection pooling via reqwest Client reuse | `crates/llm-proxy-provider/src/transport.rs:139` |
| 133 | security | extra_headers iteration order is non-deterministic (HashMap) | `crates/llm-proxy-provider/src/transport.rs:161` |
| 134 | correctness | send_stream status check reads entire body into memory for error responses | `crates/llm-proxy-provider/src/transport.rs:207` |
| 135 | performance | Unnecessary copy in check_status: bytes().await?.to_vec() | `crates/llm-proxy-provider/src/transport.rs:270` |
| 136 | test-coverage | Comprehensive integration tests using axum test servers | `crates/llm-proxy-provider/src/transport.rs:277` |
| 137 | test-coverage | Test server unwrap() on TcpListener::bind and local_addr | `crates/llm-proxy-provider/src/transport.rs:427` |
| 138 | test-coverage | start_test_server uses a fragile 50ms sleep for server readiness | `crates/llm-proxy-provider/src/transport.rs:436` |
| 139 | test-coverage | Source guard tests in every module ensure architectural boundaries | `crates/llm-proxy-provider/src/transport.rs:1110` |
| 140 | correctness | parse_tool_choice_string silently defaults unknown values to Auto | `crates/llm-proxy-provider/tests/fixture_tests.rs:336` |
| 141 | maintainability | parse_sse_frames duplicates production SseFramer logic | `crates/llm-proxy-provider/tests/fixture_tests.rs:386` |
| 142 | maintainability | SSE data field parsing uses strip_prefix("data:") which may not handle all va... | `crates/llm-proxy-provider/tests/fixture_tests.rs:403` |
| 143 | correctness | Model presence check uses substring matching on serialized JSON | `crates/llm-proxy-provider/tests/fixture_tests.rs:503` |
| 144 | performance | Unnecessary clone of content arrays for comparison | `crates/llm-proxy-provider/tests/fixture_tests.rs:793` |
| 145 | correctness | i64-to-i32 cast for usage token counts risks truncation | `crates/llm-proxy-provider/tests/fixture_tests.rs:1072` |
| 146 | api-design | No defense-in-depth validate() call before decode_request, unlike the Anthrop... | `crates/llm-proxy-server/src/routes/chat.rs:70` |
| 147 | correctness | Provider name regex allows arbitrarily long names | `crates/llm-proxy-server/src/routes/core_pipeline.rs:257` |
| 148 | maintainability | Redundant `validate_and_clean_provider_name` call after route extraction | `crates/llm-proxy-server/src/routes/core_pipeline.rs:410` |
| 149 | maintainability | Heartbeat interval is a compile-time constant with a TODO for configurability | `crates/llm-proxy-server/src/routes/core_pipeline.rs:513` |
| 150 | error-handling | oneshot receiver failure reports a possible panic but could also mean a norma... | `crates/llm-proxy-server/src/routes/core_pipeline.rs:650` |
| 151 | idiomatic-rust | Returning a raw tuple (StatusCode, Json<T>) instead of a typed response is le... | `crates/llm-proxy-server/src/routes/health.rs:37` |
| 152 | performance | server_name().to_owned() allocates a new String on every health/version request | `crates/llm-proxy-server/src/routes/health.rs:44` |
| 153 | correctness | Readiness probe always returns 200 OK regardless of actual readiness | `crates/llm-proxy-server/src/routes/health.rs:68` |
| 154 | idiomatic-rust | Handler signature uses raw axum::body::Bytes instead of axum::Json extractor | `crates/llm-proxy-server/src/routes/messages.rs:1` |
| 155 | security | Provider name validation deferred to core_pipeline, not at route boundary | `crates/llm-proxy-server/src/routes/messages.rs:31` |
| 156 | performance | Unnecessary format! allocation for request_path used only in dedup hashing | `crates/llm-proxy-server/src/routes/messages.rs:54` |
| 157 | error-handling | not_found handler silently discards body drain errors | `crates/llm-proxy-server/src/routes/mod.rs:125` |
| 158 | performance | created field in ModelCard is always hardcoded to 0 | `crates/llm-proxy-server/src/routes/models.rs:122` |
| 159 | api-design | TokenCountResponse does not match Anthropic API shape exactly | `crates/llm-proxy-server/src/routes/token_count.rs:25` |
| 160 | maintainability | Redundant Content-Type header insertion | `crates/llm-proxy-server/src/routes/token_count.rs:163` |
| 161 | maintainability | `#[must_use]` on `AppState::new` is informational only -- discarding the resu... | `crates/llm-proxy-server/src/state.rs:107` |
| 162 | maintainability | Magic number 64 for max_tokens in test request bodies | `crates/llm-proxy-server/tests/core_pipeline.rs:550` |
| 163 | maintainability | Inconsistent body size limits across tests: 64 KiB vs 1 MiB | `crates/llm-proxy-server/tests/core_pipeline.rs:795` |
| 164 | test-coverage | messages_valid_json_parses_and_validates uses overly broad status-code allowlist | `crates/llm-proxy-server/tests/integration.rs:207` |
| 165 | maintainability | llm-proxy-storage stub crate depends on llm-proxy-core but never uses it | `crates/llm-proxy-storage/src/lib.rs:1` |
| 166 | maintainability | llm-proxy-storage stub crate is intentionally empty -- no findings | `crates/llm-proxy-storage/src/lib.rs:1` |
| 167 | maintainability | Storage stub crate is an empty placeholder | `crates/llm-proxy-storage/src/lib.rs:1` |

</details>

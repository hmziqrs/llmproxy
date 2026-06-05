export const meta = {
  name: 'implement-all-13-phases',
  description: 'Implement all plan phases (0-12) with 3 audit rounds of 6 parallel auditors each, adversarial verification, and gate tests',
  phases: [
    { title: 'Phase 0: Baseline', detail: 'Verify green baseline' },
    { title: 'Phase 1: Core Types', detail: 'CoreRequest/CoreResponse/CoreEvent' },
    { title: 'Phase 2: Client Adapters', detail: 'Anthropic + OpenAI Chat adapters' },
    { title: 'Phase 3: Config & Routing', detail: 'TOML provider config' },
    { title: 'Phase 4: Transport', detail: 'ProxyClient + SSE framer' },
    { title: 'Phase 5: Provider Adapters', detail: '4 provider protocol adapters' },
    { title: 'Phase 6: Registry', detail: 'Provider registry resolution' },
    { title: 'Phase 7: AppState', detail: 'Rewrite AppState' },
    { title: 'Phase 8: /v1/messages', detail: 'Core pipeline rewrite' },
    { title: 'Phase 9: /v1/chat/completions', detail: 'Mount real route' },
    { title: 'Phase 10: CLI Config', detail: 'Replace CLI commands' },
    { title: 'Phase 11: Cleanup', detail: 'Remove old architecture' },
    { title: 'Phase 12: Fixtures', detail: 'Complete fixture coverage' },
    { title: 'Final Verification', detail: 'Full workspace gate' },
  ]
}

var IS = {
  "type": "object",
  "properties": {
    "files_created": { "type": "array", "items": { "type": "string" } },
    "files_modified": { "type": "array", "items": { "type": "string" } },
    "summary": { "type": "string" },
    "tests_pass": { "type": "boolean" },
    "issues": { "type": "array", "items": { "type": "string" } }
  },
  "required": ["files_created", "files_modified", "summary", "tests_pass"]
}

var AS = {
  "type": "object",
  "properties": {
    "issues": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "severity": { "type": "string" },
          "file": { "type": "string" },
          "line": { "type": "integer" },
          "description": { "type": "string" },
          "suggestion": { "type": "string" }
        },
        "required": ["severity", "description"]
      }
    },
    "summary": { "type": "string" },
    "passed": { "type": "boolean" }
  },
  "required": ["issues", "summary", "passed"]
}

var FS = {
  "type": "object",
  "properties": {
    "fixes_applied": { "type": "array", "items": { "type": "string" } },
    "files_modified": { "type": "array", "items": { "type": "string" } },
    "summary": { "type": "string" },
    "tests_pass": { "type": "boolean" },
    "remaining_issues": { "type": "array", "items": { "type": "string" } }
  },
  "required": ["fixes_applied", "summary", "tests_pass"]
}

var GS = {
  "type": "object",
  "properties": {
    "passed": { "type": "boolean" },
    "test_output": { "type": "string" },
    "errors": { "type": "array", "items": { "type": "string" } },
    "fixes_applied": { "type": "array", "items": { "type": "string" } }
  },
  "required": ["passed", "test_output"]
}

var P1 = "CORRECTNESS: Verify every type field, mapping rule, and invariant matches the plan exactly. Check for missing fields, wrong types, incorrect defaults, logic errors in encode/decode, wrong status codes, broken stream event sequences, and off-by-one errors in content indexes.";
var P2 = "PLAN COMPLIANCE: Check ALL guardrails and scope constraints. Verify no forbidden imports (no cross-contamination between client/provider/server crates), no scope violations, no silent field drops without tracing::warn!, all required tests exist, and deny_unknown_fields serde attributes are in place.";
var P3 = "RUST BEST PRACTICES: Review error handling (thiserror for libraries, no unwrap/expect in production, proper Result propagation), ownership (and borrow over clone), naming conventions, documentation (/// for public API, // for why-comments), clippy compliance, and idiomatic Rust patterns.";
var P4 = "AXUM and HTTP CORRECTNESS: Verify HTTP status codes (400/500/502 mapping), SSE framing (event:/data: formatting), Content-Type headers, streaming patterns (cancel-safe, abort on disconnect), state management (Arc wrapping, Clone), IntoResponse implementations, and route-specific error envelopes.";
var P5 = "TEST COVERAGE: Verify EVERY test listed in the plan exists with the exact test names. Check for missing edge cases: empty inputs, malformed JSON, invalid UTF-8, cancellation, client disconnect, oversized inputs, missing optional fields, wrong types, and boundary values.";
var P6 = "SECURITY and SECRETS: Verify API keys are NEVER printed in Debug output (manual Debug impls that redact secrets with stars), no secret leakage in error messages, proper input validation, no path traversal in config loading, and ProviderConfig/AuthHeaders/ProxyRequest all redact api_key in their Debug.";

var PSPS = [P1, P2, P3, P4, P5, P6];

function auditPrompt(rnd, idx, phaseName, planFile) {
  return "You are AUDIT ROUND " + rnd + ", PERSPECTIVE " + (idx+1) + "/6 for: " + phaseName + "\n\n"
    + "## Your Focus:\n" + PSPS[idx] + "\n\n"
    + "## Instructions:\n"
    + "1. Read the plan: docs/plan-phases/" + planFile + "\n"
    + "2. Read docs/protocol-normalization.md and docs/protocol-mini.md for architecture rules\n"
    + "3. Read ALL files created or modified for this phase (check plan Files section)\n"
    + "4. Also read related existing files (lib.rs, Cargo.toml) to check exports and deps\n"
    + "5. Compare implementation against the plan line-by-line\n"
    + "6. Run: cargo test --workspace 2>&1 | tail -30\n"
    + "7. Run: cargo clippy --all-targets --all-features --locked -- -D warnings 2>&1 | tail -30\n"
    + "8. Return ALL issues found, even minor ones. Be thorough and adversarial.\n\n"
    + "IMPORTANT: Do NOT fix issues. Only REPORT them.";
}

function fixPrompt(rnd, phaseName, planFile, allIssues) {
  var lines = [];
  for (var i = 0; i < allIssues.length; i++) {
    var iss = allIssues[i];
    lines.push("[" + iss.severity + "]" + (iss.file ? " " + iss.file + (iss.line ? ":" + iss.line : "") : "") + ": " + iss.description + (iss.suggestion ? "\n  -> Fix: " + iss.suggestion : ""));
  }
  return "FIX all issues from " + phaseName + " audit round " + rnd + ".\n\n"
    + "## Issues to fix (" + allIssues.length + " total):\n"
    + lines.join("\n\n") + "\n\n"
    + "## Instructions:\n"
    + "1. Read docs/plan-phases/" + planFile + " for the specification\n"
    + "2. Read docs/protocol-normalization.md for architectural rules\n"
    + "3. Fix EACH issue in the source files using Edit tool\n"
    + "4. Apply Rust best practices and axum patterns\n"
    + "5. Run: cargo test --workspace\n"
    + "6. Run: cargo clippy --all-targets --all-features --locked -- -D warnings\n"
    + "7. Fix any compilation errors or test failures\n"
    + "8. Commit: git add -A && git commit -m \"fix: audit round " + rnd + " fixes for " + phaseName + "\"\n\n"
    + "CRITICAL: Fix ALL issues, not just critical ones.";
}

function implPrompt(phaseName, planFile, gateCmds, extra) {
  return "IMPLEMENT " + phaseName + " fully.\n\n"
    + "## Instructions:\n"
    + "1. Read the complete plan: docs/plan-phases/" + planFile + "\n"
    + "2. Read docs/protocol-normalization.md and docs/protocol-mini.md for architecture rules\n"
    + "3. Read existing related source files to understand current code patterns\n"
    + "4. Implement ALL required types, functions, tests, and fixtures exactly as specified\n"
    + "5. Follow Rust best practices: thiserror for library errors, no unwrap in prod, proper ownership\n"
    + "6. Follow axum patterns: correct status codes, SSE framing, Arc state, IntoResponse\n"
    + "7. Add ALL tests listed in the plan with exact test names\n"
    + "8. Run gate commands: " + gateCmds + "\n"
    + "9. Fix any compilation errors or test failures until ALL pass\n"
    + "10. Commit: git add -A && git commit -m \"feat: implement " + phaseName + "\"\n"
    + (extra ? extra + "\n" : "")
    + "\nCRITICAL: Read the ENTIRE plan file. Every type field, test name, and guardrail matters.";
}

async function auditRound(rnd, phaseName, planFile) {
  var thunks = PSPS.map(function(_, i) {
    return function() {
      return agent(auditPrompt(rnd, i, phaseName, planFile), { schema: AS, label: "audit-r" + rnd + "-p" + (i+1) });
    };
  });
  var results = await parallel(thunks);
  return results.filter(Boolean);
}

async function fixRound(rnd, phaseName, planFile, allIssues) {
  if (!allIssues || allIssues.length === 0) {
    log("Round " + rnd + ": No issues to fix");
    return { fixes_applied: [], summary: "No issues", tests_pass: true, remaining_issues: [] };
  }
  return agent(fixPrompt(rnd, phaseName, planFile, allIssues), { schema: FS, label: "fix-r" + rnd });
}

async function runPhase(phaseTitle, planFile, gateCommands, extra) {
  phase(phaseTitle);
  log("Starting " + phaseTitle);

  log("Implementing " + phaseTitle + "...");
  var imp = await agent(implPrompt(phaseTitle, planFile, gateCommands, extra), { schema: IS, label: "impl" });
  log("Implementation done: " + imp.summary);

  for (var rnd = 1; rnd <= 3; rnd++) {
    log("Audit round " + rnd + " for " + phaseTitle + "...");
    var audits = await auditRound(rnd, phaseTitle, planFile);
    var allIssues = [];
    for (var a = 0; a < audits.length; a++) {
      if (audits[a] && audits[a].issues) {
        for (var j = 0; j < audits[a].issues.length; j++) {
          allIssues.push(audits[a].issues[j]);
        }
      }
    }
    var crit = 0;
    for (var k = 0; k < allIssues.length; k++) {
      if (allIssues[k].severity === "critical" || allIssues[k].severity === "high") crit++;
    }
    log("Round " + rnd + ": " + allIssues.length + " issues (" + crit + " critical/high)");

    if (allIssues.length > 0) {
      log("Fixing " + allIssues.length + " issues from round " + rnd + "...");
      var fix = await fixRound(rnd, phaseTitle, planFile, allIssues);
      log("Fix round " + rnd + ": " + (fix.fixes_applied ? fix.fixes_applied.length : 0) + " fixes applied");
    } else {
      log("Round " + rnd + ": ALL auditors passed!");
    }
  }

  log("Adversarial verification for " + phaseTitle + "...");
  var adv = await agent(
    "ADVERSARIAL VERIFICATION for " + phaseTitle + ".\n\n"
    + "Try to BREAK the implementation.\n\n"
    + "1. Read docs/plan-phases/" + planFile + "\n"
    + "2. Read ALL implemented files for this phase\n"
    + "3. Look for: subtle bugs, missing edge cases, silent data loss, stream state corruption, panics, Debug leaking secrets, serde silently ignoring fields, wrong status codes, SSE framing bugs\n"
    + "4. Run: cargo test --workspace 2>&1 | tail -30\n"
    + "5. Run: cargo clippy --all-targets --all-features --locked -- -D warnings 2>&1 | tail -30\n"
    + "6. If you find issues, FIX them immediately and commit\n"
    + "7. Return what you found and fixed",
    { schema: FS, label: "adversarial" }
  );
  log("Adversarial: " + (adv.fixes_applied ? adv.fixes_applied.length : 0) + " fixes applied");

  log("Gate verification for " + phaseTitle + "...");
  var gate = await agent(
    "GATE VERIFICATION for " + phaseTitle + ".\n\n"
    + "Execute these commands IN ORDER and verify ALL pass:\n"
    + gateCommands + "\n\n"
    + "If any fail, fix issues and re-run until all pass.\n"
    + "Commit any fixes.",
    { schema: GS, label: "gate" }
  );
  log(phaseTitle + " gate: " + (gate.passed ? "PASSED" : "NEEDS ATTENTION"));

  return { impl: imp, gate: gate };
}

// ===== PHASE 0: Verify Baseline =====
await runPhase(
  "Phase 0: Baseline",
  "phase-00-current-state-guardrails.md",
  "cargo test --workspace && cargo clippy --all-targets --all-features --locked -- -D warnings",
  "IMPORTANT: This is CHARACTERIZATION ONLY. Do NOT modify production files. Only add or update TESTS if missing. Verify workspace compiles and all tests pass."
);

// ===== PHASE 1: Core Protocol Types =====
await runPhase(
  "Phase 1: Core Types",
  "phase-01-add-core-protocol-types.md",
  "cargo test -p llm-proxy-protocol core && cargo test -p llm-proxy-protocol --test core_exports && cargo test --workspace",
  "Add crates/llm-proxy-protocol/src/core.rs. Update lib.rs. Add integration test at crates/llm-proxy-protocol/tests/core_exports.rs. CoreStreamError must NOT collide with llm_proxy_core::CoreError."
);

// ===== PHASE 2: Client Protocol Adapters =====
await runPhase(
  "Phase 2: Client Adapters",
  "phase-02-add-client-protocol-adapters.md",
  "cargo test -p llm-proxy-protocol client && cargo test --workspace",
  "Add client/mod.rs, client/anthropic.rs, client/openai_chat.rs. Add thiserror to Cargo.toml. Update anthropic.rs wire DTOs. Create fixture dirs. Client adapters must NOT import llm_proxy_provider, llm_proxy_server, or transformer."
);

// ===== PHASE 3: Provider Config & Routing =====
await runPhase(
  "Phase 3: Config & Routing",
  "phase-03-add-provider-config-and-routing-types.md",
  "cargo test -p llm-proxy-core provider_config && cargo test -p llm-proxy-core model_route && cargo test --workspace",
  "Add provider_config.rs and model_route.rs to llm-proxy-core. TOML parsing with env var interpolation. ProviderConfig MUST have custom Debug that redacts api_key. All structs use deny_unknown_fields."
);

// ===== PHASE 4: Protocol-Neutral Transport =====
await runPhase(
  "Phase 4: Transport",
  "phase-04-add-protocol-neutral-transport.md",
  "cargo test -p llm-proxy-provider transport && cargo test --workspace",
  "Add error.rs, transport.rs, sse.rs to llm-proxy-provider. Move ProviderError from client.rs. ProxyClient is protocol-neutral. SseFramer buffers partial frames. AuthHeaders/ProxyRequest must have manual Debug redacting api_key."
);

// ===== PHASE 5: Provider Protocol Adapters =====
await runPhase(
  "Phase 5: Provider Adapters",
  "phase-05-add-provider-protocol-adapters.md",
  "cargo test -p llm-proxy-provider && cargo test --workspace",
  "LARGEST phase. Add adapter/ with mod.rs, openai_chat.rs, anthropic.rs, responses.rs, gemini.rs. ProviderAdapterRegistry::builtin(). Fixture dirs for all 4 protocols. Provider adapters must NOT import client adapters or server code."
);

// ===== PHASE 6: Provider Registry Resolution =====
await runPhase(
  "Phase 6: Registry",
  "phase-06-build-provider-registry-resolution.md",
  "cargo test -p llm-proxy-core provider_registry model_route && cargo test --workspace",
  "Add provider_registry.rs. Cross-file validation: every provider exists, every model exists, every protocol known. Actionable error messages."
);

// ===== PHASE 7: Rewrite AppState =====
await runPhase(
  "Phase 7: AppState",
  "phase-07-rewrite-appstate.md",
  "cargo test -p llm-proxy-server && cargo test --workspace",
  "Update state.rs with LegacyState bridge. Add app_config, providers, provider_adapters, proxy_client fields. from_legacy/from_toml constructors. Update routes/mod.rs, health.rs, main.rs. AppState Debug must not leak API keys."
);

// ===== PHASE 8: Rewrite /v1/messages =====
await runPhase(
  "Phase 8: /v1/messages",
  "phase-08-rewrite-v1-messages.md",
  "cargo test -p llm-proxy-server messages && cargo test --workspace",
  "Rewrite messages.rs using core pipeline. Add core_pipeline.rs and error_response.rs. RouteError with ClientProtocol enum. Stream errors before first byte = HTTP error, after = in-band event. NO legacy code."
);

// ===== PHASE 9: Mount /v1/chat/completions =====
await runPhase(
  "Phase 9: /v1/chat/completions",
  "phase-09-mount-real-v1-chat-completions.md",
  "cargo test -p llm-proxy-server chat && cargo test --workspace",
  "Rewrite chat.rs. Mount POST /v1/chat/completions. Reuse core_pipeline.rs. Errors must be OpenAI-shaped. Streaming ends with data: [DONE]. NO legacy imports."
);

// ===== PHASE 10: Replace CLI Config =====
await runPhase(
  "Phase 10: CLI Config",
  "phase-10-replace-cli-config-commands.md",
  "cargo test -p llm-proxy && cargo test --workspace",
  "Update main.rs. serve requires .toml. init generates config.toml + providers/*.toml. validate checks against builtins. models lists client models. JSON config prints migration error."
);

// ===== PHASE 11: Remove Old Architecture =====
await runPhase(
  "Phase 11: Cleanup",
  "phase-11-remove-old-direct-architecture.md",
  "cargo test --workspace && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo fmt --all -- --check",
  "DELETE old code: router/, transformer/, OpenCodeClient, EndpointType, classify_endpoint, old Config structs. Make AppState fields non-optional. Remove LegacyState. Remove transformer module. Verify rg checks find NO matches."
);

// ===== PHASE 12: Complete Fixtures =====
await runPhase(
  "Phase 12: Fixtures",
  "phase-12-complete-golden-fixture-coverage.md",
  "cargo test --workspace",
  "Complete ALL fixture coverage gaps. Client and provider fixtures. Each non-stream: input.json + core.json + output.json. Each stream: input.sse + core-events.json + output.sse. Add coverage-matrix test."
);

// ===== FINAL VERIFICATION =====
phase("Final Verification");
log("Running FINAL verification gate...");
var finalGate = await agent(
  "FINAL VERIFICATION GATE.\n\n"
  + "Run ALL in order. Fix any failures.\n\n"
  + "1. cargo fmt --all -- --check\n"
  + "2. cargo clippy --all-targets --all-features --locked -- -D warnings\n"
  + "3. cargo test --workspace\n"
  + "4. cargo build --workspace --locked --release\n\n"
  + "Then negative checks (should return NO matches):\n"
  + "5. rg -n 'transformer|detect_scenario|route_for_streaming|FallbackHandler|OpenCodeClient|EndpointType|classify_endpoint|is_anthropic_model|is_gemini_model|is_responses_model|is_zen' crates apps\n"
  + "6. rg -n 'handle_openai_streaming|handle_responses_streaming|handle_gemini_streaming|spawn_proxy_task' crates/llm-proxy-server/src/routes\n\n"
  + "If rg finds matches in live code, fix and re-run.\n"
  + "Commit any final fixes.",
  { schema: GS, label: "final-verification" }
);
log("FINAL: " + (finalGate.passed ? "ALL CHECKS PASSED" : "ISSUES REMAIN"));

return { status: finalGate.passed ? "success" : "needs-review", finalGate: finalGate };

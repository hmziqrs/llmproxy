# Anti-AI-Slop Rust: Lints, Static Analysis, and CI

**Cross-checked and merged, August 2026.** Every lint name, threshold, and config block below was verified against primary sources (clippy lint index, clippy source, rustc lint listing, actual project manifests). Claims I could not verify are marked ⚠️.

---

## Part 1 — Cross-check results

### Verified against primary sources

| Claim | Status | Source |
|---|---|---|
| `restriction` must not be enabled wholesale | ✅ | Clippy docs: "should, *emphatically*, not be enabled as a whole"; dedicated `blanket_clippy_restriction_lints` exists to discourage it |
| Clippy lint count ~830 | ✅ | stable index says **822**, master says **832** — channel difference, both correct |
| `unchecked_time_subtraction` exists | ✅ | Renamed from `unchecked_duration_subtraction` in clippy PR #13800; extended to cover `Duration - Duration` |
| `excessive-nesting-threshold` defaults to 0 (lint inert) | ✅ | Clippy Lint Configuration page |
| `string_slice`, `panic_in_result_fn`, `let_underscore_future`, `unused_result_ok`, `map_err_ignore`, `assertions_on_result_states`, `ignore_without_reason` | ✅ | All real, all current |
| clippy.toml test escapes: `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-indexing-slicing-in-tests`, `allow-panic-in-tests`, `allow-dbg-in-tests` | ✅ | All exist; **all default to `false`** |
| `redundant_imports` is a rustc lint | ✅ | Allow-by-default; split out as its own lint (rust PR #123813, merged Aug 2024) |
| `allow_attributes` does not catch crate-level `#![allow]` | ✅ | By design — inner attributes are for global scale |
| Workspace lints need per-member `lints.workspace = true` | ✅ | Cargo reference |
| axum enables `implicit_clone` / `needless_pass_by_ref_mut` / `needless_pass_by_value` / `redundant_clone` | ✅ | Fetched `tokio-rs/axum/Cargo.toml` directly |
| Microsoft Pragmatic Rust Guidelines has an AI chapter | ✅ | M-SINGLE-ITEM-PATH, M-TAUTOLOGICAL-TESTS, M-RUST-SHAPED, M-NO-META-DESIGN-DOCUMENTATION |
| MS recommends `clone_on_ref_ptr`, `map_err_ignore`, `unused_result_ok`, `undocumented_unsafe_blocks`, `allow_attributes_without_reason` | ✅ | Verbatim in their `[lints.clippy]` block |

### Corrections

**1. `cognitive_complexity` — do not enable. (This corrects my earlier answer.)**
My first pass recommended `cognitive-complexity-threshold = 15`, sourced from a GitHub issue. Clippy's own source documentation now reads: *"In case you still want to use the lint, don't. The main use cases this lint still has… are to lint against heavy nesting or exceedingly long functions, both of which have dedicated lints."* It is parked in `restriction` (not nursery — I also had the group wrong) *"so as to not mislead users into using this lint as a measurement tool."* Use `excessive_nesting` and `too_many_lines` instead.

**2. `missing_lints_inheritance` is nightly-only.**
Cargo issue #15579. On stable, use the `cargo-workspace-lints` crate or a CI script to verify every workspace member opted in. Presenting it as generally available will mislead.

**3. The `indexing_slicing` ↔ `get_unwrap` contradiction is a weak example.**
Evan Schwartz's config enables both simultaneously without apparent conflict. The canonical documented contradiction inside `restriction` is `big_endian_bytes` vs `little_endian_bytes`. Use that one when arguing against blanket-enabling the group.

**4. Missing category: clippy's `cargo` group.**
`all` = correctness + suspicious + style + complexity + perf. `cargo` is *not* included, and neither is it in `pedantic`. It contains `wildcard_dependencies`, `multiple_crate_versions`, `cargo_common_metadata`, `negative_feature_names`, `redundant_feature_names` — directly relevant to agents editing `Cargo.toml`.

**5. `unsafe_code = "forbid"` beats `"warn"` against agents.**
axum uses `forbid`. Unlike `deny`, `forbid` cannot be locally overridden by an `#[allow]` — which is exactly the escape hatch you are trying to close. bevy uses `deny` because the engine genuinely needs `unsafe`.

### Unverified ⚠️

- `let_underscore_future` is (I believe) already warn-by-default in `suspicious`, making `= "deny"` a strengthening rather than an enablement. Verify before relying on it.
- "Tokio pins specific toolchains for portions of its CI" — plausible, not confirmed.

### The counterargument worth knowing

Billy Levin published a direct rebuttal to the selective-opt-in approach ("Your Clippy Config Should Be Stricter-er", Apr 2026): turn on `pedantic` **and** `restriction`, set `blanket_clippy_restriction_lints = "allow"`, then walk every warning and decide. His three arguments: an allowlist means you can never overlook a useful lint; a buggy lint is obvious if you understand what it targets; contradictory lints just require a decision, not avoidance. He also argues the friction of adoption is *good* and should not be handed to an agent, because confronting each lint forces intentionality.

This is a real fork in the road. Selective opt-in (below) is lower-risk and what most production projects actually do. The allowlist approach is defensible for a small team with high discipline.

---

## Part 2 — The merged configuration

### Tier A: strict (greenfield, or a codebase you're willing to churn)

```toml
# ============ workspace root Cargo.toml ============

[workspace.lints.rust]
unsafe_code = "forbid"              # "deny" if you legitimately need unsafe
unsafe_op_in_unsafe_fn = "deny"
unused_must_use = "deny"
missing_debug_implementations = "warn"
missing_docs = "warn"               # libraries; drop for binaries
unreachable_pub = "warn"
unused_qualifications = "warn"
unused_crate_dependencies = "warn"  # agent added a dep and never used it
redundant_imports = "warn"
redundant_lifetimes = "warn"
unused_lifetimes = "warn"
trivial_numeric_casts = "warn"
ambiguous_negative_literals = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }

[workspace.lints.clippy]

# ---- baseline groups (priority = -1 so individual keys below win) ----
all      = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
cargo    = { level = "warn", priority = -1 }   # NOT covered by `all` or `pedantic`
# nursery = { level = "warn", priority = -1 }  # optional; more false positives

# ---- 1. close the suppression escape hatch (highest anti-agent value) ----
allow_attributes = "deny"
allow_attributes_without_reason = "deny"

# ---- 2. unfinished / debug leftovers ----
dbg_macro = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "warn"

# ---- 3. panic paths ----
unwrap_used = "warn"
panic = "warn"
panic_in_result_fn = "warn"
unwrap_in_result = "warn"
get_unwrap = "warn"
indexing_slicing = "warn"           # noisy; see notes
string_slice = "warn"               # UTF-8 boundary panics
unchecked_time_subtraction = "warn"
# expect_used = "warn"              # judgement call — see notes
# arithmetic_side_effects = "warn"  # ~15% signal / 85% noise — see notes

# ---- 4. silently swallowed errors and futures ----
let_underscore_future = "deny"
let_underscore_must_use = "warn"
unused_result_ok = "warn"
map_err_ignore = "warn"
assertions_on_result_states = "warn"

# ---- 5. clone / ownership slop ----
redundant_clone = "warn"
clone_on_ref_ptr = "warn"
implicit_clone = "warn"
needless_pass_by_value = "warn"
needless_pass_by_ref_mut = "warn"
str_to_string = "warn"
inefficient_to_string = "warn"

# ---- 6. async hazards ----
await_holding_lock = "warn"
await_holding_refcell_ref = "warn"
large_futures = "warn"
rc_mutex = "warn"
# if_let_mutex = "warn"             # only pre-edition-2024

# ---- 7. unsafe hygiene ----
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "warn"
unnecessary_safety_comment = "warn"
unnecessary_safety_doc = "warn"
mem_forget = "warn"

# ---- 8. numeric correctness ----
float_cmp = "warn"
float_cmp_const = "warn"
lossy_float_literal = "warn"
cast_sign_loss = "warn"
invalid_upcast_comparisons = "warn"

# ---- 9. structure ----
excessive_nesting = "warn"          # inert without the threshold below!
too_many_lines = "warn"
ignore_without_reason = "warn"
tests_outside_test_module = "warn"
# cognitive_complexity — DO NOT ENABLE (upstream advises against it)

# ---- 10. pedantic opt-outs (high false-positive in practice) ----
type_complexity = "allow"           # universally disabled by mature projects
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
cast_precision_loss = "allow"
literal_string_with_formatting_args = "allow"  # if using structured logging
```

Every workspace member then needs:

```toml
[lints]
workspace = true
```

Cargo does **not** inherit this automatically. Guard it on stable with `cargo-workspace-lints` in CI.

### `clippy.toml` (repo root)

```toml
# Tests are a different environment; ceremonial pattern-matching in tests is noise.
# NOTE: every one of these defaults to `false`.
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
allow-dbg-in-tests = false          # debug leftovers shouldn't land in tests either

# excessive_nesting is a no-op without this (default: 0)
excessive-nesting-threshold = 4
too-many-lines-threshold = 100
too-many-arguments-threshold = 6

# Project-specific bans. The `reason` string is shown to the agent in the
# error output — this is just-in-time prompt engineering.
disallowed-methods = [
  { path = "std::env::set_current_dir", reason = "unsafe with parallel tests" },
  { path = "std::thread::sleep", reason = "use tokio::time::sleep in async code" },
]
disallowed-types = [
  { path = "std::sync::Mutex", reason = "use tokio::sync::Mutex in async contexts" },
]
disallowed-macros = [
  { path = "std::dbg", reason = "debugging leftover" },
]
```

### Tier B: pragmatic (existing codebase)

Drop `pedantic` entirely (too noisy on legacy code). Keep the rustc block minus `missing_docs`. Keep sections 1, 2, 4, 6, 7 at `warn`. Defer `indexing_slicing` and `string_slice` — they produce the most warnings by far. Ratchet module-by-module using `#[expect(..., reason = "…")]` with tracking issues, never a crate-level `#![allow]`.

---

## Part 3 — Judgement calls with real data

**`expect_used`.** Argument against enabling it: the message you pass to `expect` already documents why the invariant holds, so enabling the lint and then writing `#[expect(expect_used, reason = "…")]` duplicates the same rationale twice. `NonZeroU32::new(1).expect("1 is non-zero")` is genuinely infallible. Reasonable to leave off.

**`arithmetic_side_effects`.** Catches overflows and division by zero, but fires on every `+ - * / % <<`. Evan Schwartz measured roughly **15% real issues, 85% noise** on his codebase. Enable only if you're in a domain where silent overflow is a real threat.

**`indexing_slicing` / `string_slice`.** Will produce *many* warnings. The counter-argument is that these are exactly the panics that kill Tokio worker threads silently — the motivating bug behind Schwartz's post was `byte index 200 is not a char boundary` on a naive summary truncation, which stopped a production email job. Worth the churn.

**Why the suppression lints matter most.** The easiest way for an agent to make CI green is `#[allow(...)]`. Forcing `#[expect(..., reason = "…")]` closes that: `#[expect]` warns when the lint stops firing, so suppressions cannot silently fossilize. Microsoft's M-LINT-OVERRIDE-EXPECT says the same, noting `#[allow]` remains legitimate for generated code and macros. Residual loophole: crate-level `#![allow(...)]` is deliberately not caught. Close it with a dylint rule (`crate_wide_allow` already exists in Trail of Bits' general library) or a grep in CI.

---

## Part 4 — What lints cannot catch

Microsoft's AI chapter documents four agent failure modes with no lint coverage:

- **M-SINGLE-ITEM-PATH** — agents re-export items under multiple paths (`crate::db::Connection` *and* `crate::Connection` *and* `crate::prelude::Connection`) across refactor iterations instead of cleanly redesigning.
- **M-TAUTOLOGICAL-TESTS** — tests that restate constants or mirror the implementation's branches. They pass by construction and raise the noise floor. Their guidance: a meaningful test checks a *property* the constants satisfy (evenly spaced, monotonic), not the constants themselves.
- **M-NO-META-DESIGN-DOCUMENTATION** — agents append design journals and self-report tables ("| Rule | Applied | Where |") into user-facing crate docs. Document the end state, not the journey.
- **M-RUST-SHAPED** — Java/C#/Python architecture imported wholesale: `FooManager`, `FooFactory`, artificial traits. M-WEASEL-WORDS names `Service`, `Manager`, `Factory` as the tells. Their rule of thumb: *"any striking technical similarity between Rust and { C#, Java, Python, … } implementations is indicative of deeper architectural problems; a `throw_if_null()` never makes sense."*

Add to that: logic duplicated across modules, and semantically wrong but syntactically clean code.

**Mitigations:** `cargo-mutants` for tautological tests; `cargo-modules` for module structure; jscpd/PMD-CPD for token-level duplication; `cargo-geiger` for unsafe surface; human review for the rest. There is no substitute for reading the architecture.

---

## Part 5 — Tooling beyond clippy

**Test quality — `cargo-mutants`.** The single best defense against assertion-free tests. It injects bugs and reports which ones no test catches. Scope it: `--in-diff pr.diff` on PRs, `--shard k/N --baseline=skip` for the nightly full run. Default test timeout is the greater of 20s or 5× baseline. Note Microsoft's caveat: where tautological tests exist *to satisfy* mutation testing, skip the mutant instead of writing a fake test.

**Unsafe — `miri`.** `cargo +nightly miri test`. Catches use-after-free, OOB access, invalid uninit values, misalignment, aliasing violations, data races on executed paths. Cannot prove soundness, cannot cross FFI.

**Feature combinations — `cargo-hack`.** A genuinely Rust-specific source of fake correctness that both of us under-weighted: `--all-features` passes, individual features are broken. `cargo hack check --each-feature --no-dev-deps`, or `--feature-powerset --depth 2` for libraries. Microsoft lists it under M-STATIC-VERIFICATION.

**Dependency slop.** `cargo shear --deny-warnings` (fast, also finds misplaced deps and unlinked source files) or `cargo-machete` (fast, regex-based) or `cargo +nightly udeps` (accurate, slow, nightly — Microsoft's pick). Plus `cargo deny check` for advisories, licenses, banned crates, duplicate versions.

**Supply chain — the part their doc omits entirely.** LLMs hallucinate crate names. The USENIX Security 2025 study (Spracklen et al., 576k samples, 16 models) found 19.7% of recommended packages were hallucinations. A Rust-specific follow-up found three models exceeding 40% hallucination rates for Rust packages — an outlier against Python and JS baselines. "Slopsquatting" attackers register those names. In August 2026, `arrayref`, `internment`, and `append-only-vec` were poisoned via a typosquatted build-dependency (`proc-macro1` impersonating `proc-macro2`); the payload lived in a build script, invisible in application source, and the releases were live under two hours. **Controls that actually work:** commit `Cargo.lock` and build `--locked` (a rogue `proc-macro1` entry is obvious in a lockfile diff); a cooldown policy admitting only crate versions older than N days; explicit review of `[build-dependencies]` and build scripts; `cargo owner --list` on security-relevant deps.

**Libraries — `cargo-semver-checks`.** Agents change `&str` → `String` in a public signature, everything compiles locally, and you've broken semver. Skip for applications.

**Parsers / untrusted input — `cargo-fuzz`.** Agents implement the obvious path well and miss pathological input combinations.

**Custom architectural rules — dylint.** The highest-ceiling option and the only thing that can enforce "no `Arc<Mutex<T>>` in public APIs" or "no raw SQL outside the repository module." Cost is real: lints use unstable `rustc_private` + `clippy_utils`, so nightly upgrades break them. Trail of Bits ships example libraries including `crate_wide_allow`, `commented_code`, `overscoped_allow`, `unnamed_constant`, and `non_thread_safe_call_in_test` — all directly anti-slop.

---

## Part 6 — CI

```bash
# Pin the toolchain. With -D warnings, a new lint in a new stable release
# will otherwise fail your build without warning.
# rust-toolchain.toml: channel = "1.9x.y"

cargo fmt --all -- --check

cargo clippy --workspace --all-targets --all-features -- -D warnings
# Rust 1.97+ alternative that doesn't invalidate build caches:
#   CARGO_BUILD_WARNINGS=deny cargo clippy --workspace --all-targets --all-features

cargo test --workspace --all-features
# or: cargo nextest run --workspace --all-features

cargo hack check --workspace --each-feature --no-dev-deps
cargo shear --deny-warnings
cargo deny check
cargo build --locked                 # lockfile discipline

# conditional
cargo +nightly miri test             # if any unsafe
cargo semver-checks                  # if publishing a library
cargo mutants --in-diff pr.diff      # PR-scoped test quality
cargo fuzz run <target>              # parsers / untrusted input

# nightly schedule
cargo mutants --shard ${SHARD}/16 --baseline=skip
cargo +nightly udeps
```

Wire the fast subset into `CLAUDE.md` / `AGENTS.md` as a post-edit command block, and set `rust-analyzer.check.command = "clippy"` so lints surface in-editor while the agent works. Also: adopting a stricter config on an existing codebase is itself a good agent task (Schwartz's view) — though Levin argues the friction is pedagogically valuable and you should do it yourself.

---

## Part 7 — Ranking

| Guardrail | Anti-slop value | Noise / cost |
|---|---|---|
| `allow_attributes` + `allow_attributes_without_reason` | ★★★★★ | Very low |
| Panic-path lints (`unwrap_used`, `string_slice`, `panic_in_result_fn`) | ★★★★★ | Medium–high volume |
| Ignored-Result / dropped-Future lints | ★★★★★ | Low |
| Async lock lints | ★★★★★ | Low |
| `redundant_clone` + clone family | ★★★★★ | Low |
| `clippy::pedantic` | ★★★★★ | Medium |
| `cargo-mutants` | ★★★★★ | CPU-expensive |
| Lockfile + `--locked` + dep cooldown | ★★★★★ | Very low |
| `miri` (if unsafe) | ★★★★★ | Slow |
| `cargo-semver-checks` (libraries) | ★★★★★ | Low |
| `cargo-hack` | ★★★★☆ | Low |
| `cargo-shear` / `cargo-deny` | ★★★★☆ | Very low |
| Fuzzing (parsers) | ★★★★☆ | Expensive |
| Custom dylint rules | ★★★★★ ceiling | High maintenance (nightly churn) |
| `clippy::nursery` wholesale | ★★☆☆☆ | High |
| `clippy::restriction` wholesale | ☆☆☆☆☆ | Contradictory — see Levin for the dissent |
| `cognitive_complexity` | ☆☆☆☆☆ | Upstream says don't |

---

## Sources

- Clippy lint index (stable / master) — rust-lang.github.io/rust-clippy
- Clippy Lint Configuration — doc.rust-lang.org/clippy/lint_configuration.html
- `clippy_lints/src/cognitive_complexity.rs` + PR #14915 (doc correction)
- Clippy PR #13800 (`unchecked_time_subtraction` rename)
- `tokio-rs/axum` Cargo.toml (fetched directly)
- Microsoft Pragmatic Rust Guidelines — Universal + AI chapters
- Evan Schwartz, "Your Clippy Config Should Be Stricter" (30 Apr 2026)
- Billy Levin, "Your Clippy Config Should Be Stricter-er" (30 Apr 2026) — the dissent
- rustc allowed-by-default lint listing; rust PR #123813 (`redundant_imports`)
- Cargo issue #15579 (`missing_lints_inheritance`, nightly)
- Spracklen et al., USENIX Security 2025 (package hallucination); Rust-specific follow-up studies
- Trail of Bits dylint examples README
- mutants.rs user guide

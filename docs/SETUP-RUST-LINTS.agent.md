# Task: Configure Rust lint and quality infrastructure

You are configuring a Rust project's linting, static analysis, and CI quality gates. Follow this document in order. Do not skip steps. Do not improvise lint names.

---

## Non-negotiable constraints

Read these before doing anything. Violating them makes the task a net negative.

1. **Never add `#[allow(...)]` to make a lint pass.** Use `#[expect(lint, reason = "…")]` with a real justification, or fix the code. `#[expect]` warns when the lint stops firing, so suppressions cannot rot silently.
2. **Never add a crate-level `#![allow(...)]`.** Not for any reason. If you believe one is needed, stop and report it to the human.
3. **Never mass-suppress.** If enabling a lint produces 200 warnings and you resolve all 200 with `#[expect]`, you have achieved nothing. Follow the triage rules in Step 6.
4. **Do not enable `clippy::restriction` as a group.** Clippy's own documentation says it must emphatically not be enabled wholesale; it contains mutually contradictory lints (e.g. `big_endian_bytes` vs `little_endian_bytes`). Cherry-pick only.
5. **Do not enable `clippy::cognitive_complexity`.** Upstream clippy documentation explicitly says not to use it. Use `excessive_nesting` and `too_many_lines` instead.
6. **Do not invent lint names.** Every lint in this document has been verified to exist. If you think another lint is useful, verify it at `https://rust-lang.github.io/rust-clippy/master/index.html` before adding it, and note it in your report as an addition.
7. **Do not modify application logic in this task** beyond the mechanical fixes listed in Step 6. If a lint reveals a real bug, report it — do not silently redesign.
8. **Do not add or remove dependencies** from `Cargo.toml` `[dependencies]`. This task touches lint config, CI, and lint-driven code fixes only.

---

## Step 0 — Gather facts

Run these and record the answers. Every later decision depends on them.

```bash
# Is this a workspace? List members.
grep -A 30 '^\[workspace\]' Cargo.toml || echo "SINGLE CRATE"

# Toolchain and edition
rustc --version
cargo --version
grep -E '^(edition|rust-version)' Cargo.toml */Cargo.toml 2>/dev/null

# Does lint config already exist?
grep -rn 'lints' Cargo.toml */Cargo.toml 2>/dev/null
ls clippy.toml .clippy.toml rust-toolchain.toml rust-toolchain 2>/dev/null

# Size and age signals
find . -name '*.rs' -not -path './target/*' | wc -l
tokei . 2>/dev/null || find . -name '*.rs' -not -path './target/*' -exec cat {} + | wc -l

# Does it contain unsafe?
grep -rn 'unsafe ' --include='*.rs' . | grep -v '^./target' | wc -l

# Is it async?
grep -rn 'tokio\|async-std\|smol' Cargo.toml */Cargo.toml 2>/dev/null | head

# Library or binary?
ls src/lib.rs src/main.rs */src/lib.rs */src/main.rs 2>/dev/null

# Current warning baseline (DO NOT FIX ANYTHING YET)
cargo clippy --workspace --all-targets --all-features 2>&1 | tail -5
```

Record: workspace y/n + member list, edition, MSRV, unsafe count, async y/n, lib/bin, LOC, current warning count.

---

## Step 1 — Choose the tier

Apply this decision rule mechanically.

| Condition | Tier |
|---|---|
| Fewer than ~2000 lines of Rust, or no existing `[lints]` config and fewer than 50 current warnings | **A (strict)** |
| Everything else | **B (pragmatic)** |
| Human explicitly told you which tier | Use what they said |

If you are between tiers, choose **B**. Tier B can be ratcheted to Tier A later; a Tier A adoption that buries the team in warnings will get reverted wholesale.

State your chosen tier and the reason in your final report.

---

## Step 2 — Write the lint configuration

### If this is a workspace

Add to the **root** `Cargo.toml`. If `[workspace.lints.*]` sections already exist, merge — do not clobber existing entries without listing them in your report.

### If this is a single crate

Use the same blocks but with headers `[lints.rust]` and `[lints.clippy]` instead of `[workspace.lints.rust]` / `[workspace.lints.clippy]`, and skip Step 3.

### Tier A — strict

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
unsafe_op_in_unsafe_fn = "deny"
unused_must_use = "deny"
missing_debug_implementations = "warn"
missing_docs = "warn"
unreachable_pub = "warn"
unused_qualifications = "warn"
unused_crate_dependencies = "warn"
redundant_imports = "warn"
redundant_lifetimes = "warn"
unused_lifetimes = "warn"
trivial_numeric_casts = "warn"
ambiguous_negative_literals = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }

[workspace.lints.clippy]
# Groups. priority = -1 makes individual keys below override them.
all      = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
cargo    = { level = "warn", priority = -1 }

# 1. Suppression escape hatch
allow_attributes = "deny"
allow_attributes_without_reason = "deny"

# 2. Unfinished / debug leftovers
dbg_macro = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "warn"

# 3. Panic paths
unwrap_used = "warn"
panic = "warn"
panic_in_result_fn = "warn"
unwrap_in_result = "warn"
get_unwrap = "warn"
indexing_slicing = "warn"
string_slice = "warn"
unchecked_time_subtraction = "warn"

# 4. Silently swallowed errors and futures
let_underscore_future = "deny"
let_underscore_must_use = "warn"
unused_result_ok = "warn"
map_err_ignore = "warn"
assertions_on_result_states = "warn"

# 5. Clone / ownership slop
redundant_clone = "warn"
clone_on_ref_ptr = "warn"
implicit_clone = "warn"
needless_pass_by_value = "warn"
needless_pass_by_ref_mut = "warn"
str_to_string = "warn"
inefficient_to_string = "warn"

# 6. Async hazards
await_holding_lock = "warn"
await_holding_refcell_ref = "warn"
large_futures = "warn"
rc_mutex = "warn"

# 7. Unsafe hygiene
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "warn"
unnecessary_safety_comment = "warn"
unnecessary_safety_doc = "warn"
mem_forget = "warn"

# 8. Numeric correctness
float_cmp = "warn"
float_cmp_const = "warn"
lossy_float_literal = "warn"
cast_sign_loss = "warn"
invalid_upcast_comparisons = "warn"

# 9. Structure
excessive_nesting = "warn"
too_many_lines = "warn"
ignore_without_reason = "warn"
tests_outside_test_module = "warn"

# 10. Pedantic opt-outs (documented high false-positive rate)
type_complexity = "allow"
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
cast_precision_loss = "allow"
```

### Tier B — pragmatic

Same as Tier A with these changes:

- **Remove** the `pedantic` group line entirely (and therefore the whole section 10 opt-out block, except keep `type_complexity = "allow"`).
- **Remove** `missing_docs`, `missing_debug_implementations`, `unused_qualifications` from `[workspace.lints.rust]`.
- **Remove** `indexing_slicing` and `string_slice` from section 3. These produce the highest warning volume; add them in a follow-up pass.
- **Downgrade** every `"deny"` in sections 1, 2, 7 to `"warn"`.

### Conditional adjustments (both tiers)

Apply these based on Step 0 findings:

- **Project contains `unsafe`** → change `unsafe_code = "forbid"` to `unsafe_code = "deny"`. Keep the whole of section 7.
- **Project contains zero `unsafe`** → keep `forbid`. You may drop section 7 entirely, but leaving it costs nothing and acts as a tripwire.
- **Not async** → drop section 6.
- **Edition 2021 or earlier and async** → add `if_let_mutex = "warn"` to section 6. On edition 2024 this is unnecessary.
- **Binary application, not a library** → drop `missing_docs`. Keep everything else.
- **Library published to crates.io** → keep `missing_docs`; do not use `unsafe_code = "forbid"` if downstream needs to opt out.
- **Uses structured logging (`tracing` with message templates)** → add `literal_string_with_formatting_args = "allow"`.

---

## Step 3 — Propagate to workspace members

**Cargo does not inherit workspace lints automatically.** Every member crate must opt in. Add to each member's `Cargo.toml`:

```toml
[lints]
workspace = true
```

Verify none were missed:

```bash
for f in $(find . -name Cargo.toml -not -path './target/*' -not -path './Cargo.toml'); do
  grep -q 'workspace = true' "$f" || echo "MISSING LINT INHERITANCE: $f"
done
```

Every member must print nothing. If any member is intentionally excluded, note it in your report with the reason.

---

## Step 4 — Write `clippy.toml`

Create `clippy.toml` in the repo root (not `.clippy.toml` — pick one, and if `.clippy.toml` already exists, edit that instead of creating a second file).

```toml
# Test escapes. IMPORTANT: every one of these defaults to `false`,
# so they must be set explicitly or tests will drown in warnings.
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
allow-dbg-in-tests = false

# excessive_nesting is a NO-OP without this. Default threshold is 0.
excessive-nesting-threshold = 4
too-many-lines-threshold = 100
too-many-arguments-threshold = 6
```

Then, **only if** the project has a documented convention that justifies it, add bans. Do not invent bans. Ask the human if unsure. Example shape:

```toml
disallowed-methods = [
  { path = "std::thread::sleep", reason = "use tokio::time::sleep in async code" },
]
disallowed-types = [
  { path = "std::sync::Mutex", reason = "use tokio::sync::Mutex across await points" },
]
disallowed-macros = [
  { path = "std::dbg", reason = "debugging leftover" },
]
```

The `reason` string is surfaced in the compiler error. Write it as an instruction, not a complaint.

If the project declares an MSRV below the current stable, also add:

```toml
msrv = "1.XX"   # match rust-version in Cargo.toml
```

---

## Step 5 — Pin the toolchain

Create `rust-toolchain.toml` if it does not exist:

```toml
[toolchain]
channel = "1.XX.Y"        # the exact version from `rustc --version` in Step 0
components = ["clippy", "rustfmt"]
```

This is required. CI will run with `-D warnings`; without a pin, a new lint in a future stable release will fail the build on an unrelated PR.

---

## Step 6 — Baseline run and triage

```bash
cargo clippy --workspace --all-targets --all-features 2>&1 | tee /tmp/clippy-baseline.txt
grep -c '^warning' /tmp/clippy-baseline.txt
grep -oP '(?<=clippy::)[a-z_]+' /tmp/clippy-baseline.txt | sort | uniq -c | sort -rn
```

**If the count exceeds 300:** stop. Report the count and the top-10 histogram to the human and ask whether to drop to Tier B, or which lints to defer. Do not proceed.

**Otherwise, triage each warning by this rule:**

| Situation | Action |
|---|---|
| Mechanical fix exists (see recipes below) | Fix the code |
| `cargo clippy --fix` handles it safely | Run it, then review the diff |
| Lint is correct but fixing it changes behaviour or requires redesign | Leave it. Add to the report as "needs human decision". Do **not** suppress. |
| Lint is a genuine false positive | `#[expect(clippy::name, reason = "why this is actually correct")]` at the **narrowest possible scope** — the expression or item, never the module or crate |
| Lint reveals an actual bug | Stop. Report it separately and prominently. |

Auto-fix pass, reviewed:

```bash
cargo clippy --workspace --all-targets --all-features --fix --allow-dirty
git diff    # review every hunk before continuing
```

### Fix recipes

| Lint | Wrong | Right |
|---|---|---|
| `unwrap_used` in a fn returning `Result` | `x.unwrap()` | `x?` |
| `unwrap_used` on `Option`, no `Result` available | `x.unwrap()` | `let Some(x) = x else { return … };` |
| `indexing_slicing` | `v[i]` | `v.get(i).ok_or(…)?` |
| `string_slice` | `&s[..n]` | `s.chars().take(n).collect::<String>()` or `floor_char_boundary` |
| `map_err_ignore` | `.map_err(\|_\| MyErr)` | `.map_err(MyErr::from)` or `MyErr::Wrapped(e)` |
| `unused_result_ok` | `f().ok();` | `if let Err(e) = f() { tracing::warn!(?e, …); }` |
| `let_underscore_future` | `let _ = fut;` | `fut.await;` or `tokio::spawn(fut);` |
| `clone_on_ref_ptr` | `arc.clone()` | `Arc::clone(&arc)` |
| `str_to_string` | `s.to_string()` on `&str` | `s.to_owned()` |
| `undocumented_unsafe_blocks` | bare `unsafe { … }` | `// SAFETY: <invariant>` immediately above |
| `allow_attributes` | `#[allow(x)]` | `#[expect(x, reason = "…")]` |
| `await_holding_lock` | guard held across `.await` | scope the guard in `{ … }` before the await, or use `tokio::sync::Mutex` |
| `needless_pass_by_value` | `fn f(s: String)` | `fn f(s: &str)` |

---

## Step 7 — CI

Create or update the workflow. If a CI file already exists, merge these steps rather than replacing it.

`.github/workflows/ci.yml`:

```yaml
name: CI
on: [push, pull_request]

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2

      - name: Format
        run: cargo fmt --all -- --check

      - name: Clippy
        run: cargo clippy --workspace --all-targets --all-features -- -D warnings

      - name: Lint inheritance
        run: |
          for f in $(find . -name Cargo.toml -not -path './target/*' -not -path './Cargo.toml'); do
            grep -q 'workspace = true' "$f" || { echo "Missing [lints] workspace = true in $f"; exit 1; }
          done

      - name: Test
        run: cargo test --workspace --all-features

      - name: Locked build
        run: cargo build --locked

  # Add only if the tools are already in use or the human approved them.
  extended:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-hack cargo-shear cargo-deny --locked
      - run: cargo hack check --workspace --each-feature --no-dev-deps
      - run: cargo shear --deny-warnings
      - run: cargo deny check
```

**Conditional CI additions** — add only if the corresponding condition from Step 0 holds:

- Project contains `unsafe` → add a job running `cargo +nightly miri test`
- Project is a published library → add `cargo semver-checks`
- Project parses untrusted input → note in the report that `cargo-fuzz` is warranted; do not set it up unattended
- Human asked for test-quality gating → add `cargo mutants --in-diff pr.diff --timeout 300` on PRs

Also ensure `Cargo.lock` is committed. If it is in `.gitignore` and this is a binary/application, remove that line and commit the lockfile. If it is a library, leave the existing convention alone but say so in the report.

---

## Step 8 — Write the agent rules file

Create or append to `CLAUDE.md` / `AGENTS.md` (use whichever already exists; if neither, create `AGENTS.md`):

```markdown
## Rust quality gates

Run after every change:

    cargo fmt --all
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace --all-features

### Rules

- Never silence a lint with `#[allow(...)]`. Use `#[expect(lint, reason = "…")]` at the
  narrowest scope, or fix the code. Crate-level `#![allow(...)]` is forbidden.
- No `unwrap()`, `expect()`, `panic!`, `todo!`, or `unimplemented!` in non-test code.
  Propagate errors with `?`. Use `thiserror` for libraries, `anyhow` for applications.
- No indexing (`v[i]`) or string slicing (`&s[..n]`) in non-test code — they panic.
  Use `.get()`, and respect UTF-8 char boundaries when truncating strings.
- Never write `let _ = …` on a `Result` or a `Future`. Handle or log it.
- Every `unsafe` block needs a `// SAFETY:` comment stating the invariant.
- Never hold a `MutexGuard` across an `.await`.
- Prefer `&str` over `String` and `&[T]` over `Vec<T>` in function parameters.
- Do not add a dependency without checking it exists on crates.io, is spelled correctly,
  and is actively maintained. Verify before importing.
- Tests must assert observable behaviour. A test that restates a constant
  (`assert_eq!(RETRIES, 3)`) or mirrors the implementation's branches is worthless —
  test a property instead.
- Public items should be reachable through exactly one path. Do not add re-exports to
  paper over a refactor.
- Do not put design narratives, "why we chose X over Y" essays, or self-report tables
  into user-facing documentation.
```

---

## Step 9 — Verify

All of these must pass before you declare the task complete:

```bash
cargo fmt --all -- --check                                              # exit 0
cargo clippy --workspace --all-targets --all-features -- -D warnings    # exit 0
cargo test --workspace --all-features                                   # exit 0
cargo build --locked                                                    # exit 0
```

Plus these manual checks:

- [ ] Every workspace member has `[lints] workspace = true`
- [ ] `clippy.toml` exists and contains `excessive-nesting-threshold`
- [ ] `rust-toolchain.toml` pins an exact version
- [ ] `grep -rn '#!\[allow' --include='*.rs' src/ */src/` returns nothing you added
- [ ] Every `#[expect]` you added has a `reason` that a reviewer would accept
- [ ] `Cargo.lock` is committed
- [ ] `git diff --stat` — the change should be mostly config; a large source diff means you over-reached

**Synthetic test.** Prove the config actually fires:

```bash
# Temporarily add `let _x: i32 = None::<i32>.unwrap();` to a non-test file
cargo clippy 2>&1 | grep -q 'unwrap_used' && echo "GATE ACTIVE" || echo "GATE NOT WORKING"
# Then remove it
```

---

## Step 10 — Report

Output exactly this structure:

```
## Configuration applied

Tier: [A/B] — reason: …
Project shape: [workspace with N members / single crate], edition …, MSRV …, [async], [contains unsafe], [lib/bin]

## Files changed
- Cargo.toml — added [workspace.lints.rust] and [workspace.lints.clippy]
- <member>/Cargo.toml — added [lints] workspace = true  (× N)
- clippy.toml — created
- rust-toolchain.toml — created, pinned to X.Y.Z
- .github/workflows/ci.yml — [created / merged N steps]
- AGENTS.md — [created / appended quality gates section]

## Lint results
Baseline warnings: N
Fixed mechanically: N
Suppressed with #[expect]: N   (list each with file:line and the reason given)
Deferred lints (removed from config to keep adoption viable): …

## Needs human decision
- file:line — <lint> — <why it can't be fixed mechanically>

## Possible real bugs found
- file:line — <description>

## Recommended next steps
- <e.g. add indexing_slicing in a follow-up pass; enable cargo-mutants; set up miri>
```

If the "Suppressed with `#[expect]`" count is larger than the "Fixed mechanically" count, say so explicitly and flag it — that ratio means the config is too aggressive for this codebase and should be trimmed rather than papered over.

---

## Escalate to the human, do not decide alone

- Baseline warning count over 300
- Any lint that appears to reveal a real bug
- Any place you believe a crate-level `#![allow]` is genuinely required
- Existing lint config that conflicts with this document
- A dependency in `Cargo.toml` you cannot find on crates.io, or whose name looks like a
  near-miss of a popular crate (e.g. `proc-macro1` vs `proc-macro2`) — this is a
  supply-chain red flag, report immediately and do not build
- Any request to weaken `allow_attributes` or `allow_attributes_without_reason`

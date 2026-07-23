# Publish readiness — crates.io

Scope: everything that decides whether `cargo publish` succeeds and how the
crates present on crates.io / docs.rs. Verified against Cargo 1.94.1 on this
tree (commit `f38041e`).

## Verdict

**Publishing is currently blocked.** One hard blocker (B1) makes
`cargo publish` fail for every crate except `tex2word-ir` and
`tex2word-math`. The rest are metadata gaps that won't stop the upload but
will noticeably hurt how the crates look and are found.

Good news first: all eight names (`tex2word`, `tex2word-ir`, `tex2word-math`,
`tex2word-frontend`, `tex2word-backend`, `tex2word-latex`,
`tex2word-validate`, `tex2word-cli`) are **unclaimed on crates.io** as of
2026-07-23 (checked via the crates.io API). The workspace builds clean,
`cargo test --workspace` passes (113 tests), `cargo clippy --all-targets`
is warning-free, and `cargo fmt --check` passes.

## Blockers

### B1 — path dependencies have no `version` (publish fails)

Every inter-crate dependency is declared as `{ path = "../..." }` only.
`cargo package`/`publish` rejects this:

```
error: all dependencies must have a version requirement specified when packaging.
  dependency `tex2word-ir` does not specify a version
```

Fix in each crate manifest, e.g. `crates/tex2word-frontend/Cargo.toml`:

```toml
[dependencies]
tex2word-ir = { path = "../tex2word-ir", version = "1.0.6" }
```

Cleaner: hoist all inter-crate deps to `[workspace.dependencies]` in the root
`Cargo.toml` and use `tex2word-ir.workspace = true` in members, so the
version is stated once.

Publish order matters (leaf-first): `tex2word-ir` and `tex2word-math` →
`tex2word-frontend`, `tex2word-backend`, `tex2word-latex`,
`tex2word-validate` → `tex2word` → `tex2word-cli`.

Note: `tex2word`'s `[dev-dependencies]` (`tex2word-validate` etc.) may keep
`path` without `version` — Cargo strips versionless dev-dependencies when
packaging, which is fine here since they're only used by integration tests.

### B2 — workspace metadata is defined but never inherited

`[workspace.package]` declares `repository`, `homepage`, `description`,
`keywords`, and `categories`, but workspace-package fields are only applied
to a member that opts in with `field.workspace = true`. Today only
`tex2word`, `tex2word-latex`, and `tex2word-validate` inherit `repository`;
**no crate** inherits `keywords`, `categories`, or `homepage`. The result
(already visible as a `cargo package` warning: *"manifest has no
documentation, homepage or repository"*) is crates.io pages with no repo
link and no keyword/category indexing.

Add to every member `Cargo.toml`:

```toml
repository.workspace = true
homepage.workspace = true
keywords.workspace = true
categories.workspace = true
```

Also note each crate keeps its own hand-written `description` — that's good,
keep those (the workspace-level `description` is then unused; consider
removing it to avoid confusion). Suggestion: drop "(Rust port)" from the
descriptions — on crates.io everything is Rust, and the phrase reads as
"this is a translation, expect rough edges". Something like
"LaTeX front-end for tex2word: LaTeX -> IR" is stronger.

`keywords` has 5 entries (`latex`, `docx`, `ooxml`, `converter`, `math`) —
exactly the crates.io maximum, OK. `categories` values
(`command-line-utilities`, `text-processing`) are both valid crates.io
categories; `command-line-utilities` really only fits `tex2word-cli`, so
consider per-crate categories instead of inheriting both everywhere.

## Strongly recommended (not upload-blocking)

### R1 — no README is packaged for any crate

`cargo package --list` shows only `src/` and manifests. Without a `readme`
entry, every crates.io page will be blank. Minimum fix — point all crates at
the repo README from each member manifest:

```toml
readme = "../../README.md"   # path is relative to the crate; file gets embedded
```

Better: a short per-crate `README.md` for the library crates (what it does,
one code snippet, "part of the tex2word workspace" + link), and the full
README for `tex2word` and `tex2word-cli`, the two crates users actually land
on.

### R2 — LICENSE file is not included in any package

`license = "MIT"` (inherited) satisfies crates.io, but the LICENSE text
itself isn't shipped in any `.crate`. Standard practice (and what
license-audit tooling like `cargo-about`/`cargo-deny` looks for) is a
license file per package. Either copy `LICENSE` into each crate directory,
or add to each member manifest:

```toml
license-file = ...   # NO — don't use this together with `license`
# instead:
include = [...]      # only if you also want to trim the package; simplest is:
```

Simplest reliable approach: symlinks are not packaged portably, so copy the
7-line MIT file into each `crates/*/LICENSE` (or add
`exclude`/`include` handling). Low effort, removes a whole class of
downstream compliance friction.

### R3 — `documentation` field

Set `documentation = "https://docs.rs/<crate>"` per crate (or just rely on
docs.rs default linking — but the `cargo package` warning goes away and the
crates.io sidebar gets a Docs link even before first docs.rs build finishes).

### R4 — binary/library naming collision

`tex2word-cli` names its binary `tex2word`, and `tex2word` is also the
library crate. Locally this already trips the `cargo doc` output-filename
collision warning (cargo bug #6313). On crates.io it also means the
intuitive `cargo install tex2word` installs **nothing useful** (library-only
crate), while the real command is `cargo install tex2word-cli`.

Recommendation: give the **`tex2word` crate itself** the `[[bin]]` target
(the CLI is 138 lines with no extra deps beyond `tex2word-validate`) and
either drop `tex2word-cli` or leave it as a thin re-export. Then
`cargo install tex2word` does what everyone will try first. If you keep the
split, say prominently in the README: *"Install the CLI with
`cargo install tex2word-cli`."* Decide **before** first publish — yanking
and renaming after the fact is messy.

### R5 — docs.rs / rustdoc warnings

`cargo doc --workspace --no-deps` emits real warnings that will reproduce on
docs.rs:

| Where | Warning |
| --- | --- |
| `crates/tex2word-cli/src/main.rs:4` | unclosed HTML tag `p` (`<p>` in usage text is parsed as HTML — escape as `` `<p>` `` or use a code block) |
| `crates/tex2word-math/src/lib.rs:4` | doc links to private modules `parser` and `omml` (either `pub` them, or de-link the references) |
| `crates/tex2word-ir/src/lib.rs:76` | unresolved doc link `[n]` (escape the bracket: `\[n\]`) |
| workspace | bin `tex2word` vs lib `tex2word` doc filename collision (see R4) |

### R6 — stale/incorrect doc references

- `crates/tex2word/src/lib.rs:7` says "see `rust/ROADMAP.md`" — that path is
  from the pre-split repo; it's `ROADMAP.md` at the repo root here.
- Same doc block calls the crate "an early **vertical slice**" — accurate
  once, but this is being published as 1.0.6; the crate-level doc is the
  first thing docs.rs shows. Rewrite to describe the current feature surface
  (the README already does this well — reuse it, or use
  `#![doc = include_str!("../README.md")]` once R1 adds per-crate READMEs).

### R7 — repo hygiene

- `.gitignore` is inherited from a Python project (rules for `*.pem`,
  SQLite DBs, `__pycache__`, `.venv`, `server.sh` artifacts, `manage.py`
  keys — none exist in this repo). Harmless but confusing; trim to the Rust
  entries (`/target`, `*.docx` with the tests exception, OS junk).
- `Cargo.lock` is committed — correct choice for a workspace with a binary;
  keep it.
- There is no CI workflow in the repo. Before publishing, a minimal GitHub
  Actions job running `cargo test --workspace && cargo clippy --all-targets
  -- -D warnings && cargo fmt --check` on stable + MSRV (1.82) protects the
  "kept warning-free" claims in the README.
- `rust-version = "1.82"` is declared; we did not verify the build on 1.82
  in this review (toolchain 1.94 here). Run `cargo +1.82 check --workspace`
  once before release or lower/raise the claim accordingly.

## Suggested release checklist

1. Fix B1 + B2 (one mechanical pass over the 8 manifests).
2. Decide R4 (who owns the binary) — before first publish.
3. Add READMEs + LICENSE copies (R1, R2), fix rustdoc warnings (R5, R6).
4. `cargo publish --dry-run -p <crate>` for each crate in dependency order.
5. Tag `v1.0.6`, publish leaf-first, verify docs.rs builds.

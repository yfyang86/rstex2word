# API design & documentation review

Scope: the public surface you are about to freeze by publishing. Unlike bugs,
several of these **cannot be fixed after 1.0.6 without a semver-major bump**,
so they deserve a decision before the first `cargo publish`. Line numbers
refer to commit `f38041e`.

## Decide before first publish (semver traps)

### A1. No `#[non_exhaustive]` anywhere in `tex2word-ir`
`crates/tex2word-ir/src/lib.rs` — `Inline` (:24), `Block` (:113),
`EmphasisKind` (:12), `CiteMode` (:72), `RefKind` (:88), `RefStyle` (:104),
`TocKind` (:186), `FloatKind` (:211), `TableAlign` (:218), and the
all-public-field structs (`Document` :269, `Float` :194, `Table` :245,
`TableRow` :238, `TableCell` :226, `Theorem` :167, `BibEntry` :177,
`ListItem` :157, `LabelInfo` :253).

The crate's own docs (lib.rs:5-6) say the IR "grows module by module" — the
CHANGELOG shows exactly that (1.0.6 added variants and fields). Once
published, **every added enum variant or struct field is a breaking change**
for downstream `match`/struct-literal code. Options:

1. Mark the growing enums and structs `#[non_exhaustive]` now (costs
   downstream a `_ => …` arm and constructor functions), or
2. Accept a major bump per addition (the version number will run away from
   the reference implementation's, defeating the "shared version" scheme).

Option 1 is strongly recommended; it is the single highest-leverage change
in this review. Same question, smaller scale, for `tex2word::Conversion`
(`crates/tex2word/src/lib.rs:31-36`, all-public fields) and
`tex2word-backend::PageGeometry` (public fields, `ooxml.rs:47-56`).

### A2. Infallible APIs with no diagnostics channel
- `tex2word_math::to_omath(&str) -> String` (`math/src/lib.rs:14-16`) is the
  crate's entire API: no `Result`, no error type, and the crash cases (02 —
  C2, C3) are undocumented. Unknown commands are silently dropped
  (`parser.rs:259`), so `\undefinedcmd{x}` loses content with no way for the
  caller to detect it.
- `tex2word_backend::to_docx`/`to_docx_with` (`backend/src/lib.rs:16-24`)
  return `Vec<u8>` infallibly; missing/unreadable images silently degrade to
  a `[image: …]` text placeholder with no warning surfaced, and the fallback
  is documented only inside the private `ooxml` module.
- Consequently **no crate in the workspace defines an error type** (no
  `std::error::Error` impls at all).

The pipeline crate (`tex2word`) does have a warnings channel
(`Conversion.warnings`), which is the right shape — but the layers below it
swallow diagnostics before they can reach it. Recommendation: keep the
lenient behavior, but (a) document it on the public items themselves, and
(b) thread degradation events up (a `Vec<Warning>` out-param or return
struct) so `--strict` actually covers image/math losses.

### A3. Frontend does not re-export the IR (C-REEXPORT)
`crates/tex2word-frontend/src/lib.rs` returns `tex2word_ir::Document` etc.
but never re-exports the crate, so downstream users cannot name the return
type of `parse_document` without adding a separately-versioned dependency on
`tex2word-ir`. The umbrella crate does this right (`pub use tex2word_ir as
ir`); add the same to `tex2word-frontend` (and `tex2word-backend`, which
takes `&ir::Document` parameters).

### A4. Undocumented implicit I/O
`tex2word_frontend::parse_document` (lib.rs:25) resolves `\input`/`\include`
against the **current working directory** — implicit filesystem reads from a
function whose signature looks pure (`&str -> Document`). Document it
loudly, and consider a no-I/O variant for untrusted input (see 02 — S1/S2:
absolute paths and `../` traversal read arbitrary files).

## Documentation gaps

- **Crate-level doc rot**: `crates/tex2word/src/lib.rs:7` points at
  `rust/ROADMAP.md` (pre-split path; it's `ROADMAP.md` here) and describes
  the crate as "an early **vertical slice**" — the wrong first impression
  for a 1.0.6 docs.rs page. `tex2word-cli/src/main.rs:1` has the same
  "vertical slice" line, and its usage comment (lines 3-6) omits `--report`
  while the runtime `usage()` includes it.
- **rustdoc warnings** (reproduce on docs.rs): unclosed HTML tag `<p>` in
  `tex2word-cli/src/main.rs:4`; doc links to private modules in
  `tex2word-math/src/lib.rs:4`; unresolved `[n]` link in
  `tex2word-ir/src/lib.rs:76`. Details + fixes in `01-publish-readiness.md`
  (R5).
- **Missing item docs**: `EmphasisKind` variants (`ir/src/lib.rs:12-20`),
  `TableCell`/`TableRow`/`Table` struct-level docs (`ir/src/lib.rs:226-249`),
  `PageGeometry` per-field docs (units are only implied by the struct doc).
  No crate sets `#![warn(missing_docs)]` — adding it to the library crates
  keeps the surface honest as it grows.
- **Undocumented output characteristics**: the ZIP writer is STORE-only
  (`backend/src/zip.rs`), so `.docx` output is several times larger than
  Word's — acknowledged in an internal comment as roadmap but not in
  user-facing docs. The output also omits `docProps/*` (no
  title/author/app metadata; properties panes show blanks).
- **`Document.columns: usize` magic default** — "0 means 1"
  (`ir/src/lib.rs:278-281`). Cleaner as an explicit `Default` of 1 or
  `NonZeroUsize`; at minimum keep the doc, but it's a wart in a 1.x API.

## CLI polish

- No `--version`/`-V` flag — table stakes for a published binary
  (and the README's install story points people at `cargo install`).
- On *any* error, `main` prints the full usage block
  (`cli/src/main.rs:30-34`) — appropriate for argument errors, noisy for
  "conversion produced 3 warnings with --strict" or "file not found".
  Print usage only for usage errors.
- `main.rs:126-130`: `format!("valid ({} checks passed)", "all")` — a
  string literal fed through a numeric-looking placeholder; either count the
  checks or just write "valid".
- `latex`/`validate` subcommands accept stray flags inconsistently:
  `validate` takes exactly one positional and ignores everything after it
  (e.g. `tex2word validate a.docx b.docx` silently ignores `b.docx`).

## Test suite notes

`crates/tex2word/tests/` (5 files, all pass) exercise real end-to-end
conversion including OPC validation of the output — good. Gaps worth
closing, given the findings in `02-…`:

- No malformed-input/robustness tests (every crash in 02 was found outside
  the suite; each fix should land with its trigger as a regression test).
- No round-trip property test pairing `to_latex_source` with a re-parse
  (would have caught the m19–m23 escaping/list issues).
- The corpus-parity test is dead code in this repo — VERIFIED.
  `corpus_parity.rs:16` computes the corpus root as
  `CARGO_MANIFEST_DIR/../../../tests`, which resolves to a `tests/`
  directory **outside the repository** (the comment "rust/crates/tex2word →
  repo root" gives it away: the path is from the pre-split layout where the
  workspace lived under `rust/`). Since the root never exists here, the
  test hits the early `return` at line 32-35 and silently passes as a
  no-op — the "go/no-go gate for cutover" described in its own doc comment
  never runs. Fix the path to `../../tests`, commit the corpus fixtures (or
  fetch them in CI), and make the missing-corpus case a visible skip rather
  than a silent pass.

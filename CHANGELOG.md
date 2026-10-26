# Changelog

All notable changes to **tex2word (Rust)** are recorded here. The Rust line
tracks the reference implementation and shares its version numbers; see
[`README.md`](README.md) for the feature overview.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 2.0.0 — a faithful port of tex2word 1.0.9

The Rust line is rewritten as a port of the Python `tex2word` **1.0.9** core
install (`pip install tex2word` without extras). Earlier 1.0.x releases were an
independent reimplementation; 2.0.0 replaces it and follows the Python code
function by function.

### Parity

- For every conversion recorded from the Python test suite (431 calls), the
  IR, the conversion report and the `.docx` bytes are identical to Python's.
  The same holds for `parse_document`, `read_docx`, `to_latex` (including
  reconciliation of Word edits), `recover_ir`, `write_latex`, `validate_docx`,
  the report helpers and the CLI. The `tex2word-goldens` crate checks all of
  it, with the scripts that record the goldens in `tools/golden/`.

### Added

- Everything in the Python 1.0.9 core install that the 1.0.x port lacked,
  among it: `--reference-doc` Word templates, `--citations zotero` live
  fields, BibTeX/`.bbl` bibliographies with author-year styles, `--lang`,
  `--number-by-section`, `--columns`, the `latexml` front end,
  `--math-image-fallback` (TeX + dvipng), TikZ compilation, siunitx,
  glossaries, tcolorbox callouts, algorithms, endnotes, index fields, the
  `.docx` → LaTeX round trip with a manifest and edit reconciliation, a
  reader for foreign `.docx` files, `coverage` and `benchmark` commands, and
  HTML/JSON conversion reports.
- New crates: `tex2word-ooxml` (lxml-compatible XML, a libxml2-faithful
  parser, ZIP and a zlib-identical compressor), `tex2word-latexwalker` (a
  port of pylatexenc's walker and `latex2text`), `tex2word-bib`, and the
  unpublished `tex2word-goldens`.

### Changed

- **Breaking:** the public API follows Python's. `convert_source(source,
  base_dir, &ConvertOptions)` and `convert_file(input, output, &opts)` return
  a `ConversionResult { document, report, docx }`; the IR mirrors `ir.py`;
  `to_latex` lives in `tex2word::roundtrip`; `validate_docx` returns
  `Result<Vec<String>, ValidateError>`.
- **Breaking:** the CLI is the Python one: `convert`, `coverage`, `benchmark`
  and `to-latex`, with Python's options and messages. The 1.0.x `latex` and
  `validate` subcommands and the `--strict`/`--page` options are gone.
- `.docx` files are now DEFLATE-compressed exactly as Python's `zipfile`
  writes them.

### Not ported

Features that need Python's optional extras behave as in the core install:
PDF and TikZ rasterization (pypdfium2), the `--csl` engine (citeproc-py),
the LaTeX→MathML math path (latex2mathml), matplotlib math images, and
`render_check`.

## 1.0.6 — problem sheets, cheatsheets & plain-TeX math

Brings the Rust implementation to parity with the 1.0.6 feature surface and
promotes the workspace to a standalone project.

### Added

- **`exam` document class.** `questions`/`parts`/`subparts` and
  `\question`/`\miquestion`/`\part`/`\subpart` (dropping an optional
  `[points]`) render as nested numbered lists, so exam's `\part` no longer
  collides with `\part` sectioning ("Part I D"). A question that leads straight
  into its `\parts` still gets its own number above the `(a)/(b)/(c)`
  sub-items; solutions are hidden unless `\printanswers`; the Oxford-style
  problem-sheet title is recovered from `\course`/`\sheetnumber`/`\oxfordterm`/
  `\sheettitle`.
- **Nested lists.** `Block::List` now stores depth-tagged `ListItem`s, so
  `itemize`/`enumerate` nest to arbitrary depth. The numbering definitions gain
  five multilevel levels (`1.` / `(a)` / `(i)` / …). Display math `\[ … \]` and
  inline `\( … \)` inside a list item now render.
- **Plain-TeX math.** `\halign` systems of equations inside `\[ … \]` (wrapped
  in `\centerline{\hbox{\vbox{\openup…\jot …}}}`) convert to an `array`; `\cr`
  is treated as a matrix/array row separator; the math-class wrappers
  `\mathbin`/`\mathrel`/`\mathop`/`\mathord`/`\mathopen`/`\mathclose`/
  `\mathpunct`/`\mathinner` render their content transparently.
- **Symbols.** Normal-subgroup relations (`\vartriangleleft`/`\trianglelefteq`/
  `\ntrianglelefteq`/…), `\nmid`/`\nparallel`/`\smallsetminus`, and the
  restriction/harpoon glyphs (`\restriction`/`\upharpoonright`/…). The `\/`
  italic correction is dropped.
- **TikZ "cheatsheet" content boxes.** `\node{…minipage…}` + `\node[fancytitle]`
  idioms have their text/math recovered (titles become headings) instead of
  being dropped; real diagrams still route to the image path.

### Project

- Standalone-ready workspace: version 1.0.6 across all crates, MSRV 1.82,
  `Cargo.lock` tracked, richer package metadata.
- End-to-end test suite for the 1.0.6 features (`crates/tex2word/tests/
  feature_1_0_6.rs`) with a vendored `oxmathproblems` fixture.
- Project documentation: `README.md`, `DEVELOPMENT.md`, `CONTRIBUTING.md`.

## Earlier

Phases 0–6 built the dependency-free end-to-end pipeline: the LaTeX front-end,
the OMML math engine, tables/figures/images, live fields & cross-references
(numbered sections, TOC, multi-column), citations/footnotes/theorems, the
structural OOXML validator, the conversion/coverage report, and the IR→LaTeX
round-trip writer. See `ROADMAP.md` and the `PHASE*_PLAN.md` documents for the
phase-by-phase history.

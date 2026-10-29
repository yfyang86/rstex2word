# Changelog

All notable changes to **tex2word (Rust)** are recorded here. The Rust line
tracks the reference implementation and shares its version numbers; see
[`README.md`](README.md) for the feature overview.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 2.4.0 — 2026-10-29 — catch up with the Python 1.1.0 reference

Ports the Python tex2word **1.1.0** fixes (yfyang86/tex2word#45/#46) into the
Rust line, so the two again agree on everything the reference converts
([rstex2word#9](https://github.com/yfyang86/rstex2word/issues/9)). On the
issue's 7,980 synthetic probes, Rust 2.3.0 failed 1,711; 2.4.0 fails
@@PROBES@@ (Python 1.1.0: @@PYPROBES@@ under the same oracles), and every probe that
fails only in Rust is gone.

### Added

- **Journal front matter** for 17 classes (`frontmatter.rs`): authors with
  markers (`\author*[1]{…}`, elsarticle `\corref`/`\fnref`, llncs
  `\inst`/`\orcidID`, NeurIPS `\And`, IEEE `\IEEEmembership`, ICML
  `\icmlauthor`), structured affiliations (acmart `\institution`/`\city`,
  sn-jnl `\orgname`, elsarticle `organization={…}`, authblk `\affil[1]`),
  notes (amsart `\curraddr`/`\urladdr`/`\dedicatory`, `\cortext`/`\fntext`,
  KOMA `\subtitle`/`\publishers`), and class body commands (sn-jnl
  `\abstract{}`/`\bmhead`, KOMA `\addsec`/`\minisec`, IEEE `\appendices`,
  beamer frames, the `letter` class, moderncv entries). Every
  `\author`/`\affil` line of the preamble counts. New IR fields:
  `DocumentMeta.subtitle` and `front_notes`.
- **Table and float wrappers**: `threeparttable` (caption, label, `\tnote`),
  `sidewaystable`/`sidewaysfigure`, side-by-side captioned minipages and
  `subtable`s (one table each, "(a)"-lettered under an overall caption),
  `adjustbox`; `multicols` and `landscape` are transparent.
- **Real cached field numbers.** SEQ, STYLEREF and REF fields carry the
  numbers Word would compute (`field_results.rs`), so viewers that don't
  update fields no longer show "Figure 1" everywhere and "see 0".
  Section-relative numbering uses `STYLEREF 1 \n` and restarts each counter
  (`SEQ X \r 1 \s 1`) after every Heading 1; `\newtheorem{thm}{Theorem}[section]`
  numbers per section even in a flat document.
- **Ordered lists restart**: each `enumerate` gets its own numbering instance
  (`start=`, `\setcounter{enumi}`), and a nested list restarts under every
  parent item.
- **References**: `\ref`/`\cite` inside titles and labels, `\pageref`, lstlisting
  `caption=`/`label=` and minted `listing` floats ("Listing N"), subfig
  `\subfloat[…\label{…}]`, `\subref` → "(b)" and `\ref` to a panel → "1a".
  A target keeps its first `\label`; later ones become aliases
  (`Document.label_aliases`).
- **Macros and conditionals** (`conditionals.rs`, `counters.rs`): `\let`,
  `\csname`, `\@namedef`/`\@nameuse`, delimited `\def`, `\makeatletter`,
  `\providecommand` keeping an existing command, `\newcounter` values,
  `\newlength`/`\addtolength`, `\endinput`, `\input file` without braces,
  `\subfile`, `\newenvironment`/xparse environments; `\newif` switches,
  `\ifdefined`, `\ifthenelse`, etoolbox toggles, `\IfFileExists`,
  `\@ifpackageloaded`; the `comment` environment.
- **Inline constructs**: `\citep[p.~3]` locators render ("p. 3"),
  `\"{\i}`, `\ss` and friends, `\glqq`/`\grqq`, `\parbox`,
  `\MakeUppercase`, `\foreignlanguage`/`otherlanguage`, `\scalebox`/
  `\resizebox`, inline `tabular`, URL specials, `%` inside verbatim and
  `alltt`; lists, quotes and tables inside footnotes and margin notes become
  block footnotes.
@@MATH@@
### Fixed

- NUL and other control characters in the source no longer abort the
  conversion (they are dropped, as XML cannot carry them).

### Changed

- Where the 2.3.0 Rust code and Python 1.1.0 solved the same problem
  differently (amsart `\subjclass`, `\captionof` in table floats, TikZ
  warnings, the citation walk), Rust now follows the reference. A figure
  drawn with TikZ is reported as info, and warns only when it cannot be
  rendered.

### Golden parity

The goldens are re-recorded from Python 1.1.0: all 496 `convert_source`
and 671 `parse_document` calls give the same IR and report as Python,
@@GOLDEN@@

## 2.3.0 — 2026-10-29 — amsart manuscripts: tables, captions and formulas

Driven by a 32-page `amsart` economics manuscript (behavioural model,
Monte Carlo tables, TikZ and PDF figures). 2.2.0 converted it with 63
warnings, left raw citation keys and `??` in every table that cited a
source, printed `startsection…\baselineskip…` before each run-in heading,
dumped the landscape results table's caption as raw LaTeX, and its
formulas showed dotted placeholder boxes under every `\sum`. 2.3.0
converts it with **0 warnings**, no raw keys, no `??`, and every formula
renders.

### Fixed

- **Citations and cross-references inside tables, captions and
  theorem-like blocks resolve.** `\citet{…}` in a table cell came out as the
  bare key and `Section~\ref{…}` as `??`; the same for figure/table
  captions and `\newtheorem` environments (and text inside
  `\textcolor`/size switches). Inherited from the Python reference, whose
  passes only walked paragraphs, headings, lists and figure bodies.
- **Sums and integrals own their operand.** `\sum_{i} e_i\,\sigma` emitted
  the operator with an empty body followed by the terms — Word draws an
  empty body as a dotted placeholder box. The operand (up to the next `+`,
  `−`, relation, comma or wide space at the same level, brackets balanced)
  now sits inside the operator, nested sums included.
- **Minus is a minus sign.** `-` in math is U+2212 (as TeX sets it), not a
  hyphen; inside `\text{…}` it stays a hyphen. Reading a `.docx` back maps
  it to `-`.
- **`\\[2pt]` row spacing** in `cases`/`matrix`/`aligned` rows and between
  `align` lines no longer leaks as `[2pt]` (also `\\*`).
- **Words in math are one run.** `\mathrm{Beta}`, `\operatorname{…}` and
  `\DeclareMathOperator` names were emitted letter by letter (shown spaced
  out in some viewers, read back as italic letters); they are one upright
  run, and come back as `\mathrm{Beta}`.
- Digits, brackets, punctuation and `+`/`−` in math are plain math runs
  rather than "normal text": Word gives binary operators their spacing, and
  LibreOffice no longer shows brackets as `\(`. Relations, and an operator
  alone in a slot (`x^{+}`), keep the normal-text flag LibreOffice needs.
- `\def\paragraph{\@startsection…}` (or any structural command redefined
  through LaTeX internals in a `\makeatletter` block) no longer replaces the
  parser's native heading with raw markup.
- A landscape table — `\rotatebox{90}{\begin{minipage}…\captionof{table}{…}
  \label{…}…tabular…}` inside a `table` float — keeps its caption, label
  and rows; `\rotatebox` is transparent elsewhere too.
- The "image not embedded" warning names the tool to install (poppler's
  `pdftoppm`, or Ghostscript for EPS) instead of the Python package extra.

### Added

- amsart `\subjclass[…]{…}` prints as "*JEL classification:* D91, …" (the
  label from `\@namedef{subjclassname@JEL}{…}`, or amsart's
  "Mathematics Subject Classification").
- makecell `\makecell[…]{a\\b}` and `\thead` render as multi-line cells.
- Acceptance tests (`crates/tex2word/tests/amsart_paper.rs`): a synthetic
  MIT-licensed amsart fixture covering all of the above (on 2.2.0 it shows
  every one of these defects), and an opt-in run on the original manuscript
  (`T2W_UAT_SCISSORS=<dir>`). CI's figure job now runs both acceptance
  suites.

### Golden parity

The math changes alter the OMML of every recorded conversion with sums,
minus signs, digits or brackets in math (67 `.docx` files, 15 `omml_inline`
function rows, and the reverse-path digests that read that OMML back). Their
IR and reports are unchanged and still match Python; the expected values
were regenerated with `T2W_GOLDEN_UPDATE=1`. One recorded report changes its
missing-rasteriser hint.

## 2.2.0 — 2026-10-28 — TikZ-heavy papers

Driven by arXiv:2610.32813, a two-column paper whose figures are TikZ files
pulled in with `\input` — a pattern that is getting common as authors draw
figures with AI help. 2.1.0 converted it with 32 warnings, lost one of the
two stacked pictures in a figure, dropped two tables and left four broken
references. 2.2.0 converts it with **0 warnings**: all six figures render,
including both stacked TikZ pictures, and every reference resolves.

### Fixed

- **Every TikZ picture in a figure renders.** A `figure*` holding two
  `\input` TikZ files used to compile only the first; each picture now gets
  its own image, stacked in source order. Pictures nested in a node stay
  part of their outer picture.
- **TikZ text uses the paper's font.** A pdflatex document (no
  `fontspec`/`unicode-math`/`polyglossia`, ASCII picture) now compiles with
  pdflatex and keeps its text-font packages (`times`, `newtx*`, `lmodern`,
  `fontenc`, …), so labels no longer overflow their boxes in Latin Modern.
  Unicode documents still prefer xelatex/lualatex, now also keeping
  `fontspec`/`xeCJK` (a pattern bug inherited from the reference dropped
  them). If one engine fails, the next is tried.
- **TikZ images keep their printed size**, shrunk only when wider than the
  text column (they used to be blown up to 96-dpi pixel size, then capped).
- Figure-local `\definecolor`/`\tikzset`/`\colorlet` lines that sit outside
  the picture (e.g. just before an `\input`) reach the TikZ compile, and the
  document's directory is on `TEXINPUTS` so pictures can `\input` files.
- A picture's own `\label` (e.g. the `\foreach \label/\value in …` loop
  variable) no longer replaces the figure's label.
- When a TikZ figure renders, the parser's "figure without convertible
  graphics (e.g. TikZ)" warning is withdrawn instead of contradicting the
  output.
- **Nested `\input` paths resolve like TeX**: relative to the main
  document's directory first, then to the including file's directory.
  `generated/a.tex` doing `\input{generated/b}` used to look for
  `generated/generated/b` and silently drop the table it held.
- `\protect\input{caption-file}` no longer glues the file's first word onto
  the control word (`\protectQuality` was reported as an unknown macro and
  the caption text lost).
- A heading with several `\label`s (`\label{sec:a}\label{sec:b}`) resolves
  references to every one of them, not only the first.
- `{\small \bibliographystyle{…}\bibliography{…}}` — a bibliography inside a
  font-size group — is now found; natbib's `[numbers]` option wins over an
  author-year `.bst` such as `plainnat`, as in LaTeX.
- Rasterising a PDF/EPS figure no longer leaves `tw-raster-<pid>.png` files
  next to the source figure; it works in a private temp directory.

### Added

- url package `\path{…}` and `\nolinkurl{…}` render as monospace text.
- `\onecolumn`, `\twocolumn[…]`, `\nobalance` and `\typeout{…}` are
  accepted silently instead of being reported as unsupported.
- Acceptance tests (`crates/tex2word/tests/tikz_paper.rs`): a synthetic
  MIT-licensed fixture paper that reproduces all of the above, checked in
  CI with and without TeX, and an opt-in run on the real arXiv:2610.32813
  source (arXiv's license does not allow vendoring it):
  `T2W_UAT_ARXIV_2610_32813=<extracted e-print> cargo test -p tex2word --test tikz_paper`.
- CI job `tikz` installs TeX Live + poppler and requires the figures to
  render (`T2W_REQUIRE_TIKZ=1`).

### Golden parity

One recorded `parse_document` case changes on purpose: `\twocolumn` was a
raw "unsupported macro" passthrough and is now accepted silently. The goldens
pin a machine without external tools (`TEX2WORD_NO_EXTERNAL_TOOLS`), so the
TikZ work leaves every recorded `.docx` byte-identical.

## 2.1.0 — 2026-10-27 — math coverage beyond the 1.0.9 reference

The math engine now covers the standard LaTeX + amsmath + amssymb symbol set
instead of the reference's smaller tables. This is a deliberate, documented
divergence from byte-parity: one unsupported command used to demote the whole
equation to raw LaTeX text, so a single `\leqslant` made `\mathcal` and `\le`
in the same equation look broken. In a 544-command probe (every math command
in `amssymb`, `amsmath` and `fontmath` of TeX Live), fallbacks drop from 293
to 41 — and those 41 are internal TeX machinery (`\bfdefault`, brace
extension pieces, …) that documents never use.

### Fixed

- `\bigl<delim> … \bigr<delim>` now pairs into an `m:d` delimiter, so the
  delimiters stretch in Word; plain `\big`/`\bigm` render their delimiter.
- `\left\lvert`/`\lVert`/`\rvert`/`\rVert` (and bare `\lvert x \rvert`)
  render as `|`/`‖` instead of leaking the command name into the document as
  the literal text "lvert"; unknown delimiters now fail cleanly to the raw
  fallback instead of rendering garbage.
- `\text{…}`-style groups keep their inner blanks (`$a \text{ if } b$` no
  longer renders "aifb").
- `\overset`/`\stackrel` render as `m:limUpp` and `\underset` as `m:limLow`
  instead of a 2-row matrix hack.

### Added

- amssymb symbols: negated relations (`\nleq`, `\nsim`, `\nsubseteqq`, …),
  `\leqslant`/`\geqslant` and the slanted family, `\therefore`/`\because`,
  boxed/circled operators, `\Vdash`/`\vDash` negations, `\Subset`,
  `\square`/`\blacksquare`/`\lozenge`, `\beth`/`\gimel`/`\daleth`/`\Bbbk`,
  the tail/harpoon/loop arrow families, and the rest (~185 commands).
- Base LaTeX symbols missing from the reference tables: `\imath`/`\jmath`
  (so `\hat{\imath}` works), `\nearrow`-family diagonals, `\updownarrow`
  family, `\Diamond`, `\triangleleft`/`\triangleright`, `\sqsubset`,
  harpoons, `\lgroup`/`\rgroup`, `\lmoustache`/`\rmoustache`, `\mho`,
  `\owns`, `\amalg`, `\wr`, `\frown`/`\smile`, `\leadsto`, `\Join`.
- amsmath constructs: `\boxed` (m:borderBox), `\not <rel>` (precomposed
  negations, combining-solidus accent otherwise), `\dbinom`/`\tbinom`,
  `\genfrac`, `\prescript` and `\sideset` (m:sPre), `\pod`, `\smash`,
  `\overunderset`, `\idotsint`, the `\dots` variants (`\dotsc`, `\dotsb`,
  `\dotsm`, `\dotso`, `\dotsi`), the long arrows (`\Longrightarrow`,
  `\longmapsto`, …) and the italic capital Greeks (`\varGamma`…).
- Round trip: the OMML reader understands `m:borderBox`, `m:sPre` and
  `m:limUpp`, so those equations come back as `\boxed{…}`, `\prescript…` and
  `\overset…` from foreign documents too.
- Coverage tests (`tex2word-math/tests/math_coverage.rs`) assert that every
  entry of every symbol table parses, plus an end-to-end test
  (`tex2word/tests/math_coverage.rs`) that a document exercising all of the
  above converts with zero raw-LaTeX fallbacks and round-trips.

### Golden parity

The goldens still pin Python 1.0.9 byte for byte, except for the inputs
where this change makes the Rust output *better* (7 recorded conversions:
preserved `\text{}` blanks, `\overset` as `m:limUpp`, and an `ams-article`
case whose `\not` fallback is gone). Their expected values were regenerated
with `T2W_GOLDEN_UPDATE=1 cargo test -p tex2word-goldens` (the update modes
only touch `docx` parts / report digests; IR parity is still asserted
first). `gen_math_tables.py` preserves the hand-written `EXTENSIONS`
sections of `symbols.rs` when regenerating from a future Python reference.

## 2.0.1 — figures through system tools

(Shipped inside the 2.1.0 release; recorded here for completeness.)

- PDF figures are rasterised with `pdftoppm` (poppler; `sips` on macOS),
  EPS/PS with Ghostscript, where the Python reference needs its optional
  pypdfium2 extra.
- TikZ/PGF pictures compile with the first TeX engine on `PATH` into a
  cropped `standalone` PDF that is then rasterised.
- `TEX2WORD_NO_EXTERNAL_TOOLS=1` hides all external tools (the golden suite
  uses it to pin the Python core-install behavior);
  `TEX2WORD_TIKZ_DEBUG=1` keeps the TikZ compile directory and prints the
  engine log.

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

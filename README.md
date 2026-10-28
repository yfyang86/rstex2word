# tex2word (Rust)

Convert **LaTeX** to editable **Microsoft Word** (`.docx`) with native
OfficeMath (OMML) equations and live Word fields, and convert the `.docx` back
to LaTeX.

This is a Rust port of the Python [`tex2word`](https://pypi.org/project/tex2word/)
**1.0.9**, matching what `pip install tex2word` gives you without extras. It
has **no crate dependencies**: the XML parser, ZIP container, zlib-compatible
compressor, regex engine, LaTeX walker and the LaTeX→OMML math engine are all
in the workspace.

> **Version 2.2.0.** TikZ-heavy papers convert cleanly: every picture of a
> figure is compiled (also from `\input` files and stacked in one
> `figure*`), nested `\input` paths resolve as in TeX, and `\path`,
> multi-`\label` headings and two-column end matter are handled. 2.1.0
> extended the math engine beyond the Python 1.0.9 reference. For the inputs
> recorded from the Python test suite, the Rust port still produces the same
> IR, the same conversion report and a **byte-identical `.docx`**, except a
> few documented improvements. See [Parity with Python](#parity-with-python)
> and [`CHANGELOG.md`](CHANGELOG.md).

## Install

```bash
cargo install tex2word-cli     # the `tex2word` binary
cargo add tex2word             # the library
```

From source (Rust **1.82+**):

```bash
git clone https://github.com/yfyang86/rstex2word
cd rstex2word
cargo build --release          # -> target/release/tex2word
```

## Command line

The CLI matches the Python one, including its arguments, help text and exit
codes:

```bash
tex2word convert paper.tex -o paper.docx            # LaTeX -> .docx
tex2word convert paper.tex --report report.html     # + coverage report (.json or .html)
tex2word convert paper.tex --reference-doc journal.docx --lang en-GB
tex2word convert paper.tex --citations zotero       # live Zotero/Mendeley fields
tex2word convert paper.tex --number-by-section --columns 2
tex2word to-latex paper.docx -o roundtrip.tex       # .docx -> LaTeX (reconciles Word edits)
tex2word coverage papers/ -o coverage.html          # coverage dashboard for a folder
tex2word benchmark papers/ --fail-on-regression     # quantitative baseline
```

Run `tex2word convert --help` for every option. Setting `SOURCE_DATE_EPOCH`
makes the output fully reproducible.

## Library

```rust
use tex2word::{convert_source, ConvertOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src = r"\documentclass{article}\begin{document}Hi \textbf{there}, $x^2$.\end{document}";
    let result = convert_source(src, ".", &ConvertOptions::default())?;
    std::fs::write("out.docx", &result.docx)?;
    eprintln!("{}", result.report.summary()); // math: 1 OMML / 0 image / 0 raw; ...

    // back to LaTeX (uses the embedded IR manifest; reads foreign .docx too)
    let tex = tex2word::roundtrip::to_latex(&result.docx, true)?;

    // structural OOXML/OPC validation
    let problems = tex2word_validate::validate_docx(&result.docx)?;
    Ok(())
}
```

`convert_file(input, output, &opts)` does the same from and to disk.

## What it converts

Everything the Python 1.0.9 core install converts, including:

- **Structure**: title block and abstract, `\part`/`\chapter`/`\section`…
  with auto-numbered multilevel headings, `\appendix`, book/report classes,
  running heads, multi-column layout with full-width starred floats.
- **Math**: inline and display math, `equation`/`align`/`gather`/`cases`/
  matrices as native OMML, with live `SEQ Equation` numbers; the full
  amsmath/amssymb symbol set (`\boxed`, `\not`, `\bigl…\bigr` stretching
  delimiters, `\lvert`/`\lVert`, `\prescript`/`\sideset`, …); a fallback
  cascade (image via TeX + dvipng with `--math-image-fallback`, then raw
  LaTeX) recorded per equation in the report.
- **Live fields**: `\ref`/`\eqref`/`\pageref`/`\cref`/`\autoref`/`\nameref`,
  `SEQ` captions, `\tableofcontents`/`\listoffigures`/`\listoftables`,
  `\index`/`\printindex`, footnotes and endnotes.
- **Tables, figures, algorithms**: `tabular`/`longtable`/booktabs with spans,
  sub-figures, box-built figures, `algorithm`/`algorithmic`/`algorithm2e`.
- **TikZ and vector figures** (with system tools, see below): every
  `tikzpicture`/`pgfpicture`/`circuitikz`/`tikzcd`/`forest` in a figure is
  compiled with TeX and embedded as an image at its printed size; PDF and
  EPS figures are rasterised.
- **Citations**: BibTeX and `.bbl` bibliographies, numeric and author-year
  styles, `\nocite`, and live Zotero/Mendeley fields.
- **Macros and packages**: `\newcommand`/`\def`/`\newenvironment`, local
  `.sty` files, `siunitx`, `glossaries` acronyms, colors and tcolorbox callouts,
  theorems, CJK fonts via `xeCJK`.
- **Templates**: `--reference-doc` adopts a Word template's styles, theme,
  page geometry and headers/footers.
- **Round trip**: the IR travels inside the `.docx`; `to-latex` merges Word
  edits back in (accepting tracked changes) and also reads `.docx` files from
  other tools.

Unknown constructs never abort a conversion; they degrade and are logged in
the report.

## Parity with Python

The `tex2word-goldens` crate checks the port against outputs recorded from
the Python reference (CPython 3.12, pylatexenc 2.11, lxml 6 / libxml2 2.14,
zlib 1.3):

- **Conversion**: all 431 recorded `convert_source` calls give the same IR,
  report and `.docx` bytes; all 606 `parse_document` calls give the same IR
  and report.
- **Reverse path**: `read_docx`, `to_latex` (with and without reconciliation,
  and after simulated Word edits), `recover_ir` and `write_latex` match on
  every recorded conversion.
- **Building blocks**: the LaTeX walker, `latex2text`, macro expansion,
  preprocessing, BibTeX/`.bbl` parsing, LaTeX→OMML, the regex engine (13,560
  function-level cases), XML round-trips and libxml2's error messages for
  malformed XML (2,250 cases), zlib's compressed output, `validate_docx`, the
  report helpers and the CLI.

**Figures through system tools.** Where the Python reference needs its
optional `pdf` extra (pypdfium2), Rust drives tools that are usually already
installed with TeX:

| figure | needs |
|--------|-------|
| TikZ/PGF pictures | a TeX engine (`pdflatex`, `xelatex` or `lualatex`) with TikZ and `standalone`, plus `pdftoppm` (poppler) |
| PDF figures | `pdftoppm` (on macOS `sips` is a fallback) |
| EPS/PS figures | Ghostscript (`gs`) |

pdflatex documents compile their pictures with pdflatex and the paper's text
font; documents using `fontspec`/`unicode-math`/`polyglossia` prefer xelatex
or lualatex. Without the tools a figure degrades to its caption plus a report
warning. On Debian/Ubuntu:
`apt install texlive-latex-extra texlive-pictures texlive-fonts-recommended poppler-utils`.
`TEX2WORD_NO_EXTERNAL_TOOLS=1` hides all of them (the golden suite uses it);
`TEX2WORD_TIKZ_DEBUG=1` keeps the TikZ compile directory and prints the log.

**Not ported** (they need the optional Python extras, and Rust behaves like
the core install without them): the CSL engine for `--csl` (citeproc-py; the
built-in formatter is used with the same warning), the LaTeX→MathML
secondary math path (latex2mathml), matplotlib math images, and
`render_check` (LibreOffice + pypdfium2). External tools are used as in
Python: `latexml` for `--frontend latexml`, and `latex` + `dvipng` for math
images.

Where Python would crash with a traceback, the Rust CLI prints one
`error: ...` line and exits with the same status.

## Workspace

```
crates/
  tex2word-ir/           IR types, report, crossrefs; Python str/regex/json semantics
  tex2word-ooxml/        lxml-compatible XML tree/parser/serializer, ZIP + zlib deflate
  tex2word-latexwalker/  port of pylatexenc's LaTeX walker and latex2text
  tex2word-math/         LaTeX math -> OMML, the fallback cascade, OMML -> LaTeX
  tex2word-bib/          BibTeX/.bbl parsing and citation rendering
  tex2word-frontend/     LaTeX -> IR (preprocess, macros, parser, latexml, docx reader)
  tex2word-backend/      IR -> .docx (document, fields, numbering, templates, zotero)
  tex2word-latex/        IR -> LaTeX writer
  tex2word-validate/     structural OOXML/OPC validator
  tex2word/              pipeline, round trip, benchmark: the public API
  tex2word-cli/          the `tex2word` binary
  tex2word-goldens/      parity tests against the Python reference (not published)
tools/golden/            scripts that record the goldens from Python
```

See [`DEVELOPMENT.md`](DEVELOPMENT.md) for the architecture and test workflow,
[`CONTRIBUTING.md`](CONTRIBUTING.md) for development rules and
[`CHANGELOG.md`](CHANGELOG.md) for release history.

## Testing

```bash
cargo test --workspace            # unit, API, acceptance and golden parity tests
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

`crates/tex2word/tests/tikz_paper.rs` is the acceptance test for TikZ-heavy
papers: a synthetic fixture always runs (it checks the rendered figures when
TeX + poppler are present, the graceful path otherwise; `T2W_REQUIRE_TIKZ=1`
makes missing tools an error). To also run it on arXiv:2610.32813, extract
the paper's e-print and set `T2W_UAT_ARXIV_2610_32813` to that directory.

## License

MIT © Yifan Yang. See [`LICENSE`](LICENSE).

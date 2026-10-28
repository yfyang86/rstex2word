# Development manual

How tex2word (Rust) is put together and how to work on it. For the rules of
engagement (branching, commits, review), see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Prerequisites

- Rust **1.82 or newer** (the workspace MSRV), via [rustup](https://rustup.rs).
- Nothing else to build or test: no system libraries, no network, no crates.
  Optional: TeX Live + poppler for the TikZ/PDF figure paths (see
  [Acceptance tests](#acceptance-tests-and-external-tools)).
- To re-record goldens: the Python reference checked out next to this repo
  and a Python 3.12 environment with `tex2word` installed without extras (see
  [Goldens](#goldens)).

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets   # must be warning-free
cargo fmt --all --check                  # must be clean
```

## What this project is

A port of the Python `tex2word` **1.0.9** core install. The Python code is the
specification: behavior, messages, ordering and output bytes are meant to be
identical. When the two differ, the Rust code is wrong (unless the difference
is listed in the README's "Not ported").

That shapes how the code is written:

- **Python semantics are explicit.** `tex2word-ir` carries `pystr` (Unicode
  `isspace`/`isalpha`/`isdigit`, `int()`/`float()`/`round()`, `strip`, NFC),
  `re` (a Python-compatible regex engine, including `sub` templates),
  `json` (Python's `dumps`/`repr`/truthiness), `pyos` (`os.path`, text
  reading) and `dict` (insertion-ordered maps). Use them instead of Rust's
  near-equivalents whenever the Python code calls the Python version.
- **Strings are indexed by byte offsets**, never by character counts carried
  over from Python; convert where Python slices by code points.
- **Exceptions Python lets escape become `Result` errors** carrying the Python
  exception type and `str(exc)` (for example `ReadError`, `ValidateError`,
  `XmlError`, `MathError`). Exceptions Python catches are caught at the same
  place.
- **Third-party libraries are ported, not approximated.** `tex2word-latexwalker`
  is pylatexenc's walker and `latex2text`; `tex2word-ooxml` follows lxml's
  tree model and serializer, libxml2's parser (including its error messages
  and positions) and zlib 1.3's compressor, so `.docx` files are
  byte-identical.

## Pipeline

```
LaTeX ─► preprocess ─► macros ─► latexwalker ─► parser ─► IR ─► crossref ─► DocumentWriter ─► DocxPackage ─► .docx
         (frontend)   (frontend)  (pylatexenc)  (frontend)       (ir)       (backend + math)   (backend + ooxml)

.docx ─► recover_ir (manifest) ─┐
      └► docx_reader ───────────┴─► reconcile_blocks ─► latex writer ─► LaTeX
```

`tex2word::pipeline::convert_source` is the port of `pipeline.py`;
`tex2word::roundtrip` is `roundtrip.py`.

## Crates

| crate | Python modules | notes |
|-------|----------------|-------|
| `tex2word-ir` | `ir.py`, `report.py`, `transforms/crossref.py` | plus the Python-semantics helpers above |
| `tex2word-ooxml` | lxml, libxml2, `zipfile`/zlib, `backend/ooxml.py` | `xml` (tree + serializer), `parse`, `zip`, `deflate`, `inflate` |
| `tex2word-latexwalker` | pylatexenc `latexwalker`, `macrospec`, `latex2text` | spec tables generated from pylatexenc |
| `tex2word-math` | `mathml/latex_math.py`, `omml.py`, `symbols.py`, `cascade.py`, `imagemath.py`, `omml_reader.py` | |
| `tex2word-bib` | `bib/bibtex.py`, `bbl.py`, `render.py` | |
| `tex2word-frontend` | `frontend/*` | `preprocess`, `macros`, `colors`, `siunitx`, `algorithms`, `parser`, `latexml`, `docx_reader` |
| `tex2word-backend` | `backend/*`, `templates/*`, `bib/zotero.py` | `document` (the writer), `fields`, `package`, `reference`, `templates`, `tikz`, `images`, `raster` |
| `tex2word-latex` | `backend/latex_writer.py` | |
| `tex2word-validate` | `validate.py` | |
| `tex2word` | `pipeline.py`, `roundtrip.py`, `benchmark.py` | the public API, plus a `difflib` port |
| `tex2word-cli` | `cli.py` | argparse-compatible parsing and help text |
| `tex2word-goldens` | — | parity tests (not published) |

## Goldens

`crates/tex2word-goldens/goldens/` holds what the Python reference produced:

- `cases/<id>/` — every call the Python test suite made to a public API
  (`convert_source`, `parse_document`, `read_docx`, `to_latex`,
  `validate_docx`, …): `case.json` is the input, `out.json` Python's result.
  Binary inputs and outputs live content-addressed in `blobs/`.
- `*.jsonl` — targeted recordings: walker trees, `latex2text`, regex
  behavior, preprocessing stages, function-level results (`fn.jsonl`), XML
  round-trips and malformed-XML errors, zlib output, the reverse path over all
  recorded conversions, and the report helpers.

Each test in `crates/tex2word-goldens/tests/` replays one of these and fails
on the first difference, printing the path of the mismatch.

To re-record (after the Python reference changes), with the reference checked
out at `../yfyang86/tex2word` and its core install in a venv:

```bash
# cases/ and blobs/: run the Python test suite with the capture plugin,
# from the Python repo root
PYTHONPATH=<this repo>/tools/golden GOLDEN_OUT=<this repo>/crates/tex2word-goldens/goldens \
  python -m pytest -p capture_plugin -q
# the targeted recordings
python tools/golden/gen_walker_goldens.py
python tools/golden/gen_fn_goldens.py
SOURCE_DATE_EPOCH=1767312000 python tools/golden/gen_reverse_goldens.py
python tools/golden/gen_report_goldens.py
python tools/golden/gen_xml_goldens.py
python tools/golden/gen_xmlerr_goldens.py
python tools/golden/gen_deflate_goldens.py
```

Some Rust sources are generated from the reference and must be regenerated,
not edited: `latex2text_specs.rs`, `symbols.rs`, `unicode_norm.rs`, the
Unicode tables and the backend data (`gen_latex2text_specs.py`,
`gen_math_tables.py`, `gen_unicode_norm.py`, `gen_walker_tables.py`,
`gen_backend_data.py`).

Recordings freeze the clock: `\today` is January 2, 2026 and
`SOURCE_DATE_EPOCH=1767312000`; the tests set the same values.

## Acceptance tests and external tools

`crates/tex2word/tests/tikz_paper.rs` converts `tests/fixtures/tikz_paper`, a
synthetic paper with the constructs of TikZ-heavy arXiv papers. It always
runs; with a TeX engine and `pdftoppm` on `PATH` it also checks that every
figure renders (CI's `tikz` job installs them and sets `T2W_REQUIRE_TIKZ=1`
so missing tools fail the job). Set `T2W_UAT_ARXIV_2610_32813` to an
extracted copy of arXiv:2610.32813 to run the same checks on the real paper;
its license does not allow keeping it in the repository.

The golden tests set `TEX2WORD_NO_EXTERNAL_TOOLS=1`, so they never depend on
the tools installed on the machine. `TEX2WORD_TIKZ_DEBUG=1` keeps each TikZ
compile directory and prints the engine log.

## Changing behavior

1. Make the change in the Python reference first (or confirm the Python
   behavior you are matching).
2. Port it, keeping the structure of the Python function so the next diff is
   easy to follow.
3. Re-record the goldens and run `cargo test --workspace`.

## Conventions

- **No crate dependencies.** Keep it that way unless a dependency clearly pays
  for itself; discuss first.
- **Deterministic output.** The same input and `SOURCE_DATE_EPOCH` give the
  same `.docx` bytes as Python. No hash-map iteration order, no timestamps.
- **Never abort on unsupported input** where Python doesn't: degrade and log
  to the `ConversionReport`.
- **clippy- and rustfmt-clean** before every commit.

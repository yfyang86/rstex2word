# Roadmap

## 2.0.0: parity with the Python reference (done)

tex2word (Rust) 2.0.0 is a port of the Python `tex2word` **1.0.9** core
install (`pip install tex2word` without extras), checked by the golden
parity tests in `crates/tex2word-goldens`:

- the same IR, conversion report and **byte-identical `.docx`** on every
  recorded conversion;
- the reverse path (`read_docx`, `to_latex` with reconciliation,
  `recover_ir`, `write_latex`), `validate_docx`, the report helpers, the
  benchmark and the CLI;
- the third-party behavior tex2word depends on: pylatexenc's walker and
  `latex2text`, Python's `re`/`str`/`json` semantics, lxml/libxml2 (including
  error messages) and zlib's compressor.

The 1.0.x Rust line was an independent reimplementation; 2.0.0 replaces it.
The public API changed accordingly (see `CHANGELOG.md`).

## Next

- **Track Python releases.** Port each new reference release and re-record
  the goldens; keep the version numbers' major line at 2.x while the core
  install is matched.
- **Optional extras**, each behind the same switches Python uses:
  - rasterizing PDF figures and compiled TikZ (Python: pypdfium2);
  - a CSL engine for `--csl` (Python: citeproc-py);
  - the LaTeX→MathML secondary math path (Python: latex2mathml).
- **`render_check`**: the LibreOffice render smoke test (Python: soffice +
  pypdfium2).

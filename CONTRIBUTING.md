# Contributing & development rules

Thanks for working on tex2word (Rust). This document is the rulebook; the
[`DEVELOPMENT.md`](DEVELOPMENT.md) manual explains the architecture, the
golden tests and how behavior changes flow from the Python reference.

## Ground rules

1. **The Python reference is the specification.** tex2word (Rust) is a port of
   the Python `tex2word` core install. A behavior change starts in the Python
   code (or is confirmed against it), then gets ported, and the goldens are
   re-recorded. Differences from Python are bugs unless documented in the
   README.
2. **No crate dependencies without discussion.** The workspace is
   dependency-free so it builds and tests offline and deterministically.
   Adding a crate is a design decision; open an issue first.
3. **Byte-identical, deterministic output.** The same input and
   `SOURCE_DATE_EPOCH` must give the same `.docx` bytes as Python. Don't
   introduce timestamps, hash-map iteration order or other nondeterminism.
4. **Never abort where Python doesn't.** Degrade the same way: drop or
   approximate the construct and log it to the conversion report.
5. **Don't edit generated files.** Regenerate them with the scripts in
   `tools/golden/`.

## Before you commit

CI enforces these on stable and on the MSRV (1.82):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets   # warning-free
cargo test --workspace
```

Add tests for what you change: a unit test beside the code, and golden
coverage (re-recorded from Python) for user-visible behavior.

## Commits & branches

- Work on a feature branch; do not commit directly to `main`.
- Write focused commits with an imperative subject line and a body explaining
  the *why*.
- Reference the issue or the Python change you are porting.

## Pull requests

- Describe what changed and why, and which Python version or commit it
  matches.
- Include the test evidence (`fmt`/`clippy`/`test` green, goldens
  re-recorded if needed).
- Keep PRs reviewably small.

## Versioning

The project follows [Semantic Versioning](https://semver.org). The version is
set once in the workspace (`[workspace.package].version`, plus the matching
`version` on each entry of `[workspace.dependencies]`) and inherited by every
crate. Bump it there and add a `CHANGELOG.md` entry.

## Code style

- Idiomatic Rust; let `rustfmt` and `clippy` decide the small stuff.
- Mirror the structure of the Python function you are porting, and name the
  Python function in the doc comment, so the next port is a small diff.
- Comment the *why*, especially where Python semantics differ from Rust's
  (code points vs bytes, `str.isdigit` vs `char::is_ascii_digit`, ...).

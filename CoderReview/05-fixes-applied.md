# Fixes applied

Every reproduced crash, corruption, and DoS from the review is fixed, each with
a regression test. Workspace state after the changes: **`cargo test --workspace`
green (all suites), `cargo clippy --all-targets` clean, `cargo fmt --check`
clean.** The crash/DoS fixes were additionally re-verified against their
original triggers.

## Crashes & aborts on user input

| ID | Where | Fix |
| --- | --- | --- |
| C1 | `tex2word-frontend` `parse_blocks` | Added `MAX_BLOCK_DEPTH` (128); past it, remaining content is kept as a flat paragraph instead of recursing. No more stack-overflow abort on unbalanced `\begin{…}`. |
| C2 | `tex2word-math` parser + delim/matrix sub-parses | Added `MAX_DEPTH` (256) threaded through the fresh sub-parsers (`\left…\right`, matrix cells, `\sqrt[]`) so nesting is bounded across parser boundaries. |
| C3 | `tex2word-math` `read_raw_group` | Clamp the escaped-char skip so a trailing `\` can't push the cursor past the end; `\begin{x\` no longer panics. |
| C4 | `tex2word-validate` `zipread` | Replaced the two unchecked `bytes[off..]` slices with `bytes.get(..)` + checked offset arithmetic; malformed `.docx` returns an error instead of panicking. |
| C5/C6 | `tex2word-backend` footnotes | An image inside a footnote now degrades to the `[image: …]` text placeholder (footnotes.xml lacks the drawing namespaces and an image-relationship part), so the package is never corrupt. |
| C7 | `tex2word-backend` + `tex2word-math` escape | XML-illegal control characters (0x00–0x1F except tab/LF/CR) are dropped in all escape paths instead of emitted verbatim. |
| C8 | `tex2word-math` OMML escape | `"` is now escaped (`&quot;`), so a user-controlled delimiter can't break the `m:begChr`/`m:endChr` attribute. |

## Denial-of-service on small inputs

| ID | Where | Fix |
| --- | --- | --- |
| D1 | `tex2word-frontend` `macros::expand` | Added `MAX_EXPANDED_LEN` (8 MiB); a billion-laughs macro chain stops expanding and returns the partial text. |
| D2 | `tex2word-frontend` `parse_colspec` | Added `MAX_COLUMNS` (1024) bounding `*{n}{…}` repetition and its nesting. |
| D3 | `tex2word-frontend` list nesting | Added `MAX_LIST_DEPTH` (32); `level` (a `u8`) no longer overflows and recursion is bounded — deeper nesting is flattened at the cap. |

## Correctness

| ID | Where | Fix |
| --- | --- | --- |
| M3 | `tex2word` `crossref::rewrite_blocks` | Added `Block::List` and `Block::Bibliography` arms, so `\ref`/`\cite` inside list items and `\bibitem` bodies resolve (and unresolved ones now warn, so `--strict` catches them). |
| M4 | `tex2word-validate` `direct_children_of` | `depth` now uses `saturating_sub` and bails at 0; extra close-tags no longer underflow (was a debug panic / release mis-scan). |
| M5 | `tex2word-validate` `wellformed` | Truncated `<?…`/`<!--…`/`<…` now report "unterminated …" instead of silently passing validation. |
| M6 | `tex2word-frontend` `strip_comments` | Made comment-stripping `\verb`-aware, so `%` inside `\verb|…|` is preserved (was dropping the delimiter content and the rest of the line). |
| M7 | `tex2word-frontend` `detect_columns` | `\twocolumn` is matched as a complete command, so `\twocolumngrid` no longer forces a two-column layout. |
| M8 | `tex2word-validate` `normalize_join` | A leading `/` in a relationship `Target` is treated as package-root-absolute, fixing false "target does not exist" violations. |

## Deliberately deferred (documented, not code-changed)

- **`#[non_exhaustive]` on the IR** (A1) — a one-way semver decision with a real
  trade-off against the port's internal exhaustiveness checking. See the
  decision box in `04-publishing-guide.md`.
- **ZIP64 / 4 GiB+ output** (M1) — the STORE-only writer truncates past 65,535
  entries or 4 GiB. No realistic document hits this; left as a roadmap item.
- **Image extent clamping** (M2), the many round-trip `tex2word-latex` escaping
  gaps (m19–m23), and the smaller `m*` items remain open; they don't threaten
  document validity for normal input. Worth a follow-up pass.
- **Corpus-parity test path** (03) — points outside the repo and no-ops; fix
  needs the corpus fixtures committed or fetched in CI.

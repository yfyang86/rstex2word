# Code review — crates.io publication readiness

**Repository:** `yfyang86/rstex2word` at commit `f38041e`, version 1.0.6
**Date:** 2026-07-23
**Scope:** all 8 workspace crates (~9,300 lines), reviewed for correctness,
robustness, API design, and crates.io publish readiness. Every crash/DoS
claim was reproduced against this tree with a concrete input unless marked
SUSPECTED.

## Documents

| File | Contents |
| --- | --- |
| [`01-publish-readiness.md`](01-publish-readiness.md) | `cargo publish` blockers, metadata, packaging, docs.rs, release checklist |
| [`02-correctness-and-robustness.md`](02-correctness-and-robustness.md) | Crashes, corrupt-output bugs, DoS, security — with reproduced triggers |
| [`03-api-design-and-docs.md`](03-api-design-and-docs.md) | Semver traps, API shape, documentation, CLI polish, test-suite gaps |

## Verdict

**Not ready to publish yet — but close, and the foundation is genuinely
good.** The workspace builds clean, all 113 tests pass, clippy and fmt are
spotless, the parser survived extensive malformed-input and multi-byte
fuzzing without a single UTF-8 slicing panic, the hand-rolled ZIP/CRC and
image parsers are bounds-safe on hostile bytes, and all 8 crate names are
still unclaimed on crates.io. What stands between this tree and a solid
1.0.6 release is one mechanical publish blocker, a short list of reproduced
crashes that contradict the README's "never aborting" promise, and one
semver decision that can only be made before first publish.

## The blocking list

1. **`cargo publish` fails today** — path dependencies carry no `version`
   (01 — B1). One mechanical pass over the manifests; also inherit the
   workspace `repository`/`keywords`/`categories` while there (01 — B2).
2. **Reproduced crashes/aborts on malformed input** (02 — C1–C4): two
   uncatchable stack-overflow aborts (frontend environments, math nesting),
   an out-of-bounds panic in the math parser (`\begin{x\`), and two
   out-of-bounds panics in the validator's ZIP reader on corrupt `.docx`
   input. All have small triggers and localized fixes (depth caps,
   `get()` instead of indexing).
3. **Reproduced corrupt-output bugs** (02 — C5–C8): footnotes containing
   images produce an invalid package (unbound XML prefixes + dropped media
   parts); XML-illegal control characters and unescaped `"` in OMML
   attributes both yield files Word rejects.
4. **Reproduced DoS on tiny inputs** (02 — D1–D3): billion-laughs macro
   expansion (1.6 KB input), `*{2000000000}{c}` column specs (30 bytes),
   and a `u8` list-level overflow. Each needs a cap + degradation warning.
5. **Semver decision** (03 — A1): `tex2word-ir`'s enums/structs have no
   `#[non_exhaustive]`, yet the IR is documented as still growing. This is
   the one thing that cannot be retrofitted after first publish.

## High-value but not blocking

- File-disclosure via `\input{/etc/passwd}` and `\includegraphics{../…}` —
  document loudly or gate it before advertising conversion of untrusted
  documents (02 — S1/S2).
- Cross-references inside list items and bibliographies silently resolve to
  dead REF fields with no warning, so `--strict` misses them (02 — M3).
- The corpus-parity test — described in its own comments as the "go/no-go
  gate" — points outside the repository and has been silently passing as a
  no-op (03, verified).
- No README/LICENSE ships in any `.crate`; crates.io pages would be blank
  (01 — R1/R2). Decide who owns the `tex2word` binary name before publish —
  `cargo install tex2word` currently installs nothing useful (01 — R4).
- ZIP writer silently truncates past 65,535 entries / 4 GiB (02 — M1).

## Suggested order of work

1. Fix manifests (01 — B1/B2) and make the `#[non_exhaustive]` call (03 — A1).
2. Fix the eight reproduced crash/corruption bugs (02 — C1–C8), each with
   its trigger as a regression test.
3. Add the three DoS caps (02 — D1–D3).
4. Packaging polish: READMEs, LICENSE copies, rustdoc warnings, doc rot,
   `--version` flag (01 — R1–R6, 03).
5. `cargo publish --dry-run` per crate in dependency order, tag, publish
   leaf-first: `tex2word-ir`, `tex2word-math` → `tex2word-frontend`,
   `tex2word-backend`, `tex2word-latex`, `tex2word-validate` → `tex2word` →
   `tex2word-cli`.

Items 2–3 are ~10 small, well-localized fixes; nothing found in this review
suggests structural problems with the architecture.

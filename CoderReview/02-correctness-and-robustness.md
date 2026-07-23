# Correctness & robustness findings

Scope: bugs that produce wrong/corrupt output, crashes, or resource blow-ups.
Every finding is tagged **VERIFIED** (reproduced against this tree with a
concrete input, mostly in debug *and* release builds) or **SUSPECTED**
(established by careful code reading, not executed). Line numbers refer to
commit `f38041e`.

The project's own contract makes several of these release-blocking: the README
promises *"Unsupported constructs degrade gracefully … never aborting the
conversion"*, so panics and aborts on malformed input are broken promises, not
just nits.

---

## Critical — crashes and aborts on user input

### C1. Stack-overflow abort on repeated/unclosed environments (frontend) — VERIFIED
`crates/tex2word-frontend/src/lib.rs:585` (also 544, 565). `parse_blocks`
recurses for every unknown/`quote`/theorem environment with no depth limit,
and an unclosed `\begin{env}` makes `read_env_body` return the rest of the
document, so N repeated begins recurse N deep.
**Trigger:** `"\begin{a}".repeat(10_000)` (~90 KB) → `fatal runtime error:
stack overflow` (abort, **not catchable** with `catch_unwind`), debug and
release. The same shape is quadratic (each level re-collects the remaining
source into a fresh `Vec<char>`): 1,000 nestings ≈ 195 ms already.
**Fix:** explicit depth counter with graceful bail-out (degrade to text +
warning), and avoid re-materializing the tail per level.

### C2. Stack-overflow abort on deeply nested math — VERIFIED
`crates/tex2word-math/src/parser.rs:94-127` (`parse_row` ↔ `parse_atom` ↔
`parse_command`, no depth limit) and `omml.rs:51` (`render` recursion).
**Triggers (release, 8 MiB main stack):** `"\frac{1}{"` × 10,000;
`"{"` × 30,000; `"\left("` × 10,000 → abort. ~3,000 deep still passes, so the
threshold is lower on the 2 MiB stacks typical of spawned/server threads.
**Fix:** depth cap in the parser and renderer.

### C3. Out-of-bounds panic on truncated math environment — VERIFIED
`crates/tex2word-math/src/parser.rs:366` (root cause) / `431` (panic site).
In `read_raw_group` the escape branch `'\\' => self.i += 2` can push the
cursor to `len + 1`; `read_env_body`'s fallback `self.s[start..]` then panics.
**Trigger:** `to_omath("\\begin{x\\")` → `range start index 10 out of range
for slice of length 9`.

### C4. Out-of-bounds panics in the validator's ZIP reader — VERIFIED (×2)
`crates/tex2word-validate/src/zipread.rs:35` and `:63`. Both index `bytes[off..]`
with offsets taken verbatim from the file (EOCD central-directory offset;
central-directory local-header offset). Any corrupt/truncated `.docx` fed to
the public `validate_docx()` or `tex2word validate` panics instead of
returning violations.
**Triggers:** EOCD with CD offset `0x00FFFFFF` → panic; CD record whose
local-header offset points past EOF → panic. Both profiles.
**Fix:** replace the two direct slices with the `bytes.get(..)` pattern the
rest of the file already uses.

### C5. Corrupt `.docx` when a footnote contains an image — VERIFIED
`crates/tex2word-backend/src/ooxml.rs:1029-1032`. `render_footnotes_xml`
declares only the `w:`/`m:` namespaces, but an image in a footnote emits
`wp:`/`a:`/`pic:`/`r:` elements → `word/footnotes.xml` has unbound prefixes;
Word reports the file as corrupt.
**Trigger:** `\footnote{\includegraphics{pic.png}}`.

### C6. Media first referenced from a footnote is dropped from the package — VERIFIED
`crates/tex2word-backend/src/ooxml.rs:985-1015`. Image relationships,
content-type defaults, and media parts are collected **before**
`render_footnotes_xml` runs, so a footnote-only image emits
`r:embed="rId3"` with no relationship, no part, no content-type. Also no
`word/_rels/footnotes.xml.rels` is ever written, and OPC relationship IDs
resolve per-part, so footnote images can never resolve even when present.

### C7. XML-illegal control characters pass through `escape()` — VERIFIED
`crates/tex2word-backend/src/ooxml.rs:191-203` and (same class)
`crates/tex2word-math/src/omml.rs:7-18`. Chars invalid in XML 1.0
(0x00–0x08, 0x0B, 0x0C, 0x0E–0x1F) are copied verbatim into `<w:t>`.
**Trigger:** a form feed or backspace surviving from the LaTeX source, e.g.
`Inline::Text("a\x0Cb")` → `word/document.xml` is not well-formed; Word
rejects the file. **Fix:** strip or numeric-reference-invalidate these in
both escape functions.

### C8. `"` not escaped in OMML attribute values — VERIFIED
`crates/tex2word-math/src/omml.rs:7-18` (escape lacks `"`) used in attribute
position at `:116-118` (`m:begChr`/`m:endChr`); `read_delim`
(`parser.rs:448-451`) lets the user pick any delimiter character.
**Trigger:** `\left" x \right"` → `<m:begChr m:val="""/>` — malformed XML,
corrupt document part.

---

## Critical — denial-of-service on small inputs

### D1. Exponential macro expansion (billion-laughs) — VERIFIED
`crates/tex2word-frontend/src/macros.rs:264-312`. `expand` substitutes every
call per pass and iterates up to `MAX_DEPTH = 32` passes, allowing up to 2³²
growth. **Trigger:** 24 chained doubling `\newcommand`s (~1.6 KB of input)
exceed 20 s (n=20 already 2.7 s). **Fix:** cap total expanded size and/or
expansion count, degrade with a warning.

### D2. Unbounded `*{n}` column-spec repetition — VERIFIED
`crates/tex2word-frontend/src/lib.rs:917-925`. `parse_colspec` loops `0..n`
for user-supplied `n`; `*` recursion makes nesting exponential.
**Triggers:** `\begin{tabular}{*{2000000000}{c}}` (30 bytes) → >20 s / ~2 GB;
nested `*{9}{…}` ×10 (~60 bytes ≈ 9¹⁰ columns) hangs. **Fix:** cap the
repeat count and total column count.

### D3. List-nesting level overflows `u8` — VERIFIED
`crates/tex2word-frontend/src/lib.rs:836`. 300 nested `itemize` →
`attempt to add with overflow` panic in debug; in release the level wraps
(items observed at level 0 after 255), corrupting list structure. **Fix:**
`saturating_add` + clamp to the supported depth.

---

## Security — file disclosure via untrusted documents

Both crates advertise converting arbitrary LaTeX; anyone using them on
uploaded/untrusted documents gets local-file disclosure. At minimum this
needs prominent documentation; ideally an opt-in "sandboxed" mode.

### S1. `\input`/`\include` read arbitrary paths — SUSPECTED (by `Path::join` semantics)
`crates/tex2word-frontend/src/lib.rs:379-394` (with 345-376).
`base_dir.join(name)` with an **absolute** name ignores `base_dir`, so
`\input{/etc/passwd}` embeds that file's contents in the output. Relative
`../` traversal works too. `parse_document` (lib.rs:25) additionally does
this implicit I/O against the current working directory, undocumented at the
API level.

### S2. `\includegraphics` path traversal — VERIFIED
`crates/tex2word-backend/src/ooxml.rs:142`. `MediaRegistry::resolve` does
`fs::read(base_dir.join(path))` with no containment check; `\includegraphics
{../../secret.png}` embeds files from outside the source tree into the
`.docx` (reproduced).

---

## Major

### M1. ZIP writer silently truncates at format limits — VERIFIED (entry count)
`crates/tex2word-backend/src/zip.rs:103-106` (also 55-57, 67-68, 95). No
ZIP64; entry count written as `len() as u16` (65,600 images → EOCD says 70,
reproduced), sizes/offsets as `as u32` (≥ 4 GiB wraps — SUSPECTED, by
inspection). Corruption is silent; either return an error at the limits or
implement ZIP64.

### M2. Image extents not clamped to OOXML `ST_PositiveCoordinate` — VERIFIED
`crates/tex2word-backend/src/image.rs:116-127` + `ooxml.rs:353-374`.
`\includegraphics[scale=1e300]{p.png}` → `cx="18446744073709551615"`;
`width=99999999cm` → 35,999,999,640,000; both schema-invalid (max
27,273,042,316,900), Word flags/repairs. `width=0cm` yields a degenerate
`cx="0"` drawing.

### M3. Cross-references inside lists and bibliographies are silently dead — VERIFIED
`crates/tex2word/src/crossref.rs:248-281` (`rewrite_blocks`) and `:107-138`
(`collect`) have no `Block::List` / `Block::Bibliography` arms, while
`Block::Table` cells *are* handled. A `\label` in a list item is never
registered; a `\ref`/`\cite` in a list item or `\bibitem` body is never
resolved — and **no warning is recorded**, so `--strict` doesn't catch it
(reproduced: `bookmark: None`, `warnings: 0`).

### M4. Validator's XML depth tracking underflows — VERIFIED
`crates/tex2word-validate/src/lib.rs:361`. `depth -= 1` with more close-tags
than opens inside a tracked block: debug panics (`attempt to subtract with
overflow`); release wraps to `usize::MAX` and silently produces wrong child
lists. **Fix:** `saturating_sub` + bail at 0.

### M5. Validator passes truncated XML — VERIFIED
`crates/tex2word-validate/src/lib.rs:78-97`. `wellformed()` returns `None`
("well-formed") when it runs off an unterminated `<?…`, `<!--…`, or `<…`
because the internal `find(…)?` propagates as the function's own `None`.
`<w:document><?trunc` is not flagged.

### M6. `strip_comments` destroys `\verb` content containing `%` — VERIFIED
`crates/tex2word-frontend/src/lib.rs:397-420`. Comment stripping runs before
any verb/verbatim awareness: `\verb|a%b| tail` yields `"a"`; the rest of the
line is lost.

### M7. `detect_columns` substring false-positive — VERIFIED
`crates/tex2word-frontend/src/lib.rs:293`. `src.contains("\\twocolumn")`
matches longer names: a body containing `\twocolumngrid` (revtex) switches a
plain `article` to two-column layout.

### M8. Absolute OPC relationship targets mis-resolved — VERIFIED
`crates/tex2word-validate/src/lib.rs:394-407`. `Target="/word/styles.xml"`
(legal OPC, package-root-absolute) is joined onto the base dir →
`word/word/styles.xml` → false "target does not exist" violation.

---

## Minor

| # | Where | Finding | Status |
|---|---|---|---|
| m1 | `frontend/src/lib.rs:1261-1265` | TeX-primitive `$$x$$` mangled: pairs `$`…`$` as empty math, formula leaks as literal text | VERIFIED |
| m2 | `frontend/src/lib.rs:637-645` | Mid-paragraph `\label{…}` silently discarded (only attached when the paragraph buffer is empty), leaving `\ref`s dangling | VERIFIED |
| m3 | `frontend/src/macros.rs:291-294` | Sequential `body.replace("#N", arg)` re-substitutes markers contained in earlier arguments (`\f{#2}{X}` → `XX`) | VERIFIED |
| m4 | `frontend/src/lib.rs:1823-1844` | `read_optional` doesn't track nested `[`: `\caption[a[b]c]{X}` truncates and desyncs | SUSPECTED |
| m5 | `frontend/src/lib.rs:34-39` | `parse_document_reporting` runs the whole pipeline twice (once for the unsupported list, once for the parse) | VERIFIED (by reading) |
| m6 | `math/src/parser.rs:392-410` | Starred matrix envs: `\end{pmatrix*}` never matches (scan runs to EOF) and the `[r]` optional leaks into the first cell as literal text | VERIFIED |
| m7 | `math/src/parser.rs:504-521` | `\\` splits matrix rows even inside a braced group (row-break check precedes the depth check that guards `&`) | VERIFIED |
| m8 | `math/src/parser.rs:136-144` | `x^2^3` silently overwrites the first superscript, no diagnostic | SUSPECTED |
| m9 | `math/src/parser.rs:486-492` | `flatten_text` drops non-text children: `\text{rate \frac{1}{2}}` loses the fraction | SUSPECTED |
| m10 | `math/src/omml.rs:21-26` | Space-significant plain runs (e.g. from `\pmod`) lack `xml:space="preserve"` | SUSPECTED |
| m11 | `math/src/parser.rs:330-351` | `\left…\right` and env bodies re-extract + re-parse inner text per nesting level — O(depth·n); cheap adversarial input combined with C2 | SUSPECTED |
| m12 | `backend/src/ooxml.rs:704-711` | `Heading5..9` styles emitted but only Heading1-4 defined in `styles_xml()` → deep headings render as Normal; level-0 clamping inconsistency also drops `numPr` | VERIFIED |
| m13 | `backend/src/ooxml.rs:683` | Literal tab character inside `<w:t>` in bibliography instead of `<w:tab/>` element | SUSPECTED |
| m14 | `backend/src/ooxml.rs:317-327`, `1040-1053` | Nested footnotes emit `w:footnoteReference` inside `footnotes.xml`, which Word doesn't support | SUSPECTED |
| m15 | `backend/src/zip.rs:12-29` | CRC-32 table rebuilt per archive member; plus O(n²) media dedup (`ooxml.rs:139`) and per-image byte clone (2× peak memory, `ooxml.rs:997-1000`); dedup is by literal path string so `a.png` ≠ `./a.png` | VERIFIED |
| m16 | `backend` package output | No `docProps/core.xml`/`app.xml`, `word/settings.xml`, `fontTable.xml`; Word accepts but properties panes/tooling expect them | VERIFIED |
| m17 | `tex2word/src/crossref.rs:40-59` | `sanitize_bookmark` collides distinct labels (`fig:a` vs `fig.a` → `fig_a`; 40-char truncation) → duplicate bookmark names, ambiguous REFs, no warning | VERIFIED |
| m18 | `tex2word/src/crossref.rs:88-105` | Duplicate `\label{x}` silently overwrites the earlier target, no diagnostic | SUSPECTED |
| m19 | `latex/src/lib.rs:224-228, 254-261, 199-204, 288` | Round-trip writer emits labels, cite keys, image paths/options, and raw math verbatim (no escaping); `}`/`$`/`%` in these break the output (`escape()` is only applied to `Inline::Text`) | SUSPECTED |
| m20 | `latex/src/lib.rs:78-105` | List reconstruction merges items with different `ordered` flags into one environment and opens nested envs without an intervening `\item` on level jumps (invalid LaTeX) | SUSPECTED |
| m21 | `latex/src/lib.rs:158-163` | Only the first header row gets `\midrule`; multi-row headers lose header status on round-trip | SUSPECTED |
| m22 | `latex/src/lib.rs:72-76` | Unnumbered labeled `MathBlock` writes `\label` *outside* `\[…\]`, binding it to the section counter on re-parse | SUSPECTED |
| m23 | `latex/src/lib.rs:211` | `theorem()` derives the env name via `to_lowercase()` of the display kind: `kind: "Main Theorem"` → `\begin{main theorem}` (invalid) | SUSPECTED |

---

## Explicitly checked and clean

Worth recording, because it's the hard part of a dependency-free design:

- **No UTF-8 byte-slicing panics anywhere** — frontend and math parser are
  `Vec<char>`-indexed throughout; ~50 malformed/truncated/multi-byte probes
  (lone `\`, unclosed `$`/`{`/env, truncated `\verb`, `\newcommand{`, emoji,
  CJK) ran clean.
- **`image.rs` is panic-free on untrusted bytes** — all slice accesses
  bounds-guarded; a 4,000-iteration structured fuzz (PNG/JPEG/GIF prefixes,
  hostile graphicx options, truncated segments) through `to_docx` produced no
  panics.
- **ZIP writer layout and CRC-32 are correct within u16/u32 limits**
  (`unzip -t` and Python `zipfile` verify clean).
- **XML text/attribute escaping of the `&`, `<`, `>`, `"` set is correct in
  the backend** (incl. URLs and bookmark names; hyperlink `instrText` strips
  embedded quotes) — the gaps are the control-character class (C7) and the
  math crate's attribute escaping (C8).
- **CLI error handling is clean**: missing/unreadable files → `error:`
  message + exit code 1, no panics; `-h`/`--help`/no-args → usage.
- The many probed malformed math inputs (`\sqrt[` unterminated, `\hat` at
  EOF, stray `\right)`, unclosed `\begin{pmatrix}` …) all degrade without
  panicking, except the cases listed above.

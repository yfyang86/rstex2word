"""Record pylatexenc node trees as goldens for tex2word-latexwalker.

    python tools/golden/gen_walker_goldens.py   # run with the reference venv

For every recorded ``convert_source``/``parse_document`` golden case this
reproduces the first steps of the Python ``parse_document`` -- preprocessing,
macro expansion, document split, theorem/tcolorbox discovery, context build --
and records the node tree pylatexenc produces for the exact body string the
Python parser walks, plus the raw source. The document's own ``tex2word``
context category (which depends on its ``\\newtheorem``/``\\newtcolorbox``
declarations) is exported alongside, so the Rust test can rebuild the same
context without the frontend.

Output: ``crates/tex2word-goldens/goldens/walker.jsonl`` -- line 1 is the list of
distinct context categories ``[[macros, envs], ...]``; every further line is one
input ``{"src": str, "ctx": <index>, "nodes": [...]}``. Node positions
are UTF-8 byte offsets (the Rust walker's unit), converted from Python's
code-point offsets.
"""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

from pylatexenc.latexwalker import (
    LatexCharsNode, LatexCommentNode, LatexEnvironmentNode, LatexGroupNode,
    LatexMacroNode, LatexMathNode, LatexSpecialsNode, LatexWalker,
)

from tex2word.frontend import parser as P
from tex2word.frontend.macros import expand_macros, local_package_sources
from tex2word.frontend.preprocess import (
    normalize_spacing_and_icons, preprocess, replace_inline_tikz,
)

ROOT = Path(__file__).resolve().parents[2]
GOLDENS = ROOT / "crates" / "tex2word-goldens" / "goldens"


def byte_offsets(s: str) -> list[int]:
    """offs[i] = UTF-8 byte offset of code point i (offs[len(s)] = total)."""
    offs, n = [0], 0
    for ch in s:
        n += len(ch.encode("utf-8"))
        offs.append(n)
    return offs


def dump(nodes, offs):
    def pl(n):
        return offs[n.pos], offs[n.pos + n.len] - offs[n.pos]

    def args(argd):
        if argd is None:
            return None
        out = {"spec": argd.argspec, "n": [one(a) for a in argd.argnlist]}
        vt = getattr(argd, "verbatim_text", None)
        if vt is not None:
            out["v"] = vt
        return out

    def one(n):
        if n is None:
            return None
        p, l = pl(n)
        if isinstance(n, LatexCharsNode):
            return {"t": "C", "p": p, "l": l, "c": n.chars}
        if isinstance(n, LatexGroupNode):
            return {"t": "G", "p": p, "l": l, "d": list(n.delimiters), "n": [one(x) for x in n.nodelist]}
        if isinstance(n, LatexCommentNode):
            return {"t": "K", "p": p, "l": l, "c": n.comment, "s": n.comment_post_space}
        if isinstance(n, LatexMacroNode):
            return {"t": "M", "p": p, "l": l, "m": n.macroname, "s": n.macro_post_space,
                    "a": args(n.nodeargd)}
        if isinstance(n, LatexEnvironmentNode):
            return {"t": "E", "p": p, "l": l, "e": n.environmentname,
                    "n": [one(x) for x in n.nodelist], "a": args(n.nodeargd)}
        if isinstance(n, LatexSpecialsNode):
            return {"t": "S", "p": p, "l": l, "c": n.specials_chars, "a": args(n.nodeargd)}
        if isinstance(n, LatexMathNode):
            return {"t": "X", "p": p, "l": l, "d": n.displaytype, "de": list(n.delimiters),
                    "n": [one(x) for x in n.nodelist]}
        raise TypeError(type(n))

    return [one(n) for n in nodes]


def ctx_category(ctx):
    cat = ctx.d["tex2word"]
    macros = sorted((n, s.args_parser.argspec) for n, s in cat["macros"].items())
    envs = sorted((n, s.args_parser.argspec) for n, s in cat["environments"].items())
    return [macros, envs]


def materialize(case: dict, tmp: Path) -> str:
    """Write the case's snapshot files; return the base_dir to use."""
    base = tmp / "base"
    for rel, sha in case.get("files", {}).items():
        if rel.startswith("base/"):
            dest = base / rel[len("base/"):]
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes((GOLDENS / "blobs" / sha).read_bytes())
    return str(base) if case.get("base_dir") == "base" else "."


def main() -> None:
    seen: set[str] = set()
    rows = []
    contexts: list[list] = []
    ctx_index: dict[str, int] = {}
    for case_dir in sorted((GOLDENS / "cases").iterdir()):
        case = json.loads((case_dir / "case.json").read_text())
        if case["api"] not in ("convert_source", "parse_document", "expand_macros"):
            continue
        source = case["args"].get("source")
        if not isinstance(source, str):
            continue
        with tempfile.TemporaryDirectory() as td:
            base_dir = materialize(case, Path(td))
            expanded = replace_inline_tikz(expand_macros(preprocess(source, base_dir), base_dir))
            expanded = normalize_spacing_and_icons(expanded)
            body, _preamble = P._split_document(expanded)
            theorem_src = expanded + "\n" + local_package_sources(source, base_dir)
            custom, _unnum, _shared = P._collect_newtheorems(theorem_src)
            tcb_envs, _styles = P._collect_tcolorbox_defs(theorem_src)
            ctx = P._build_context(
                tuple(custom), tuple((n, P._tcb_argspec(e)) for n, e in tcb_envs.items()))
        cat = ctx_category(ctx)
        ck = json.dumps(cat)
        if ck not in ctx_index:
            ctx_index[ck] = len(contexts)
            contexts.append(cat)
        for s in (body, source):
            key = json.dumps([s, cat])
            if key in seen:
                continue
            seen.add(key)
            nodes, _, _ = LatexWalker(s, latex_context=ctx, tolerant_parsing=True).get_latex_nodes()
            rows.append({"src": s, "ctx": ctx_index[ck], "nodes": dump(nodes, byte_offsets(s))})
    out = GOLDENS / "walker.jsonl"
    with out.open("w") as fh:
        fh.write(json.dumps(contexts, separators=(",", ":")) + "\n")
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n")
    print(f"wrote {len(rows)} walker goldens -> {out}")


if __name__ == "__main__":
    main()

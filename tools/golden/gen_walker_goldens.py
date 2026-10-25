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

Also writes ``stages.jsonl``: per case, the string after each frontend stage
(``preprocess``, ``expand_macros``, ``replace_inline_tikz``,
``normalize_spacing_and_icons``, then the ``_split_document`` body/preamble),
``null`` where a stage left its input unchanged -- a debugging aid that
localises a divergence to one stage.

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
    P._today_text = lambda: "January 2, 2026"  # as capture_plugin freezes it
    seen: set[str] = set()
    rows = []
    stages = []
    parses = []
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
            pre = preprocess(source, base_dir)
            exp = expand_macros(pre, base_dir)
            tikz = replace_inline_tikz(exp)
            expanded = normalize_spacing_and_icons(tikz)
            body, _preamble = P._split_document(expanded)
            chain = [source, pre, exp, tikz, expanded]
            stages.append({
                "case": case_dir.name,
                "pre": pre, "exp": None if exp == pre else exp,
                "tikz": None if tikz == exp else tikz,
                "norm": None if expanded == tikz else expanded,
                "body": body, "preamble": _preamble,
            })
            args = case["args"]
            if case["api"] == "convert_source" and args.get("frontend", "pure") == "pure" \
                    and not args.get("csl"):
                # parse_document as convert_source runs it (pre-crossref)
                doc, rep = P.parse_document(source, base_dir)
                row = json.dumps({"case": case_dir.name, "ir": doc.to_dict(),
                                  "report": json.loads(rep.to_json())},
                                 ensure_ascii=False, sort_keys=True, separators=(",", ":"))
                if base_dir != ".":
                    row = row.replace(base_dir, "{BASE}")
                parses.append(row)
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
    with (GOLDENS / "stages.jsonl").open("w") as fh:
        for st in stages:
            fh.write(json.dumps(st, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n")
    print(f"wrote {len(stages)} stage goldens -> {GOLDENS / 'stages.jsonl'}")
    with (GOLDENS / "parse.jsonl").open("w") as fh:
        for row in parses:
            fh.write(row + "\n")
    print(f"wrote {len(parses)} parse goldens -> {GOLDENS / 'parse.jsonl'}")


if __name__ == "__main__":
    main()

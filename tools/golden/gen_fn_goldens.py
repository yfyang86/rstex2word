"""Function-level differential goldens for the pure frontend string functions.

The API-level goldens only reach some rewrite stages through a handful of
cases. This runs each pure ``str -> str`` frontend function over chunks of
every ``.tex`` file in the Python test tree (whole files, paragraph windows and
single paragraphs) and records Python's output, so the Rust port can be
checked against a much wider input set.

Writes ``crates/tex2word-goldens/goldens/fn.jsonl``: one row per call,
``{"f": name, "in": input, "out": output}``. Rows whose output equals the
input are thinned (kept for at most 25% of rows per function) to keep the
file small while still checking the no-op path.

Run from the Python repo root:
    /home/user/py-core/bin/python /home/user/rstex2word/tools/golden/gen_fn_goldens.py
"""

from __future__ import annotations

import glob
import json
import os
import random
import resource
import signal
import sys

from tex2word.frontend import macros as M
from tex2word.frontend import preprocess as P

OUT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "crates", "tex2word-goldens", "goldens", "fn.jsonl",
)

FUNCS = {
    "strip_comments": P.strip_comments,
    "normalize_listing_envs": P.normalize_listing_envs,
    "rewrite_mathoperators": P._rewrite_mathoperators,
    "strip_iffalse": P.strip_iffalse,
    "normalize_begin_star": P._normalize_begin_star,
    "recover_tikz_boxes": P._recover_tikz_boxes,
    "rewrite_halign": P._rewrite_halign,
    "inject_exam_title": P._inject_exam_title,
    "replace_inline_tikz": P.replace_inline_tikz,
    "normalize_spacing_and_icons": P.normalize_spacing_and_icons,
    "expand_macros": lambda s: M.expand_macros(s, "."),
    "collect_macros_stripped": lambda s: M.collect_macros(s)[1],
}


def chunks(text: str) -> list[str]:
    paras = [p for p in text.split("\n\n") if p.strip()]
    out = [text]
    for w in (1, 4):
        for i in range(0, len(paras), w):
            out.append("\n\n".join(paras[i : i + w]))
    return out


class _Timeout(Exception):
    pass


def _alarm(*_):
    raise _Timeout


def guarded(fn, s):
    """Call fn(s); None if Python itself blows up (time/memory) on the input."""
    signal.signal(signal.SIGALRM, _alarm)
    signal.alarm(10)
    try:
        return fn(s)
    except (_Timeout, MemoryError, RecursionError):
        return None
    finally:
        signal.alarm(0)


COMMON = [" ", "  ", "\n", "\n\n", "\t", "x", "word", "{", "}", "[", "]", "\\", "%",
          "\\%", "\\x", "\\textbf", "é", "中", "$", "#1", "#2", ",", "*", "=", "\\\\"]
VOCAB = {
    "strip_comments": ["% c\n", "%", "\\%", "\n  ", "\\\\%"],
    "normalize_listing_envs": ["\\begin{lstlisting}", "\\end{lstlisting}", "\\begin{minted}{py}",
                               "\\end{minted}", "[language=C]", "\\begin{Verbatim}", "\\end{Verbatim}"],
    "rewrite_mathoperators": ["\\DeclareMathOperator", "\\DeclareMathOperator*", "{\\argmax}",
                              "{arg\\,max}", "\\op"],
    "strip_iffalse": ["\\iffalse", "\\fi", "\\else", "\\ifx", "\\if", "\\fill", "\\elsewhere",
                      "\\iftrue", "\\ifdefined"],
    "normalize_begin_star": ["\\begin*{x}", "\\begin {itemize}", "\\begin{a*}", "\\end*{x}"],
    "recover_tikz_boxes": ["\\begin{tikzpicture}", "\\end{tikzpicture}", "\\node", "[draw]",
                           "(a)", "at (0,0)", "{text}", ";", "\\draw"],
    "rewrite_halign": ["\\halign", "{#\\hfil&#\\cr", "\\cr", "&", "\\noalign", "#", "\\hfil"],
    "inject_exam_title": ["\\title{T}", "\\begin{document}", "\\maketitle", "\\firstpageheader",
                          "\\begin{questions}", "\\documentclass{exam}"],
    "replace_inline_tikz": ["\\tikz", "\\tikz[baseline]", "{\\node{3};}", "\\begin{tikzpicture}",
                            "\\end{tikzpicture}", "\\node[circle]", "{12}", "\\draw (0,0);", "21"],
    "normalize_spacing_and_icons": ["\\vskip 2pt", "\\hskip", "\\faCheck", "\\faIcon{star}",
                                    "\\kern", "1em", "\\vspace{2pt}", "\\fa", "\\hfill",
                                    "plus 1fil", "\\faGithub", "\\quad"],
    "expand_macros": ["\\newcommand", "\\renewcommand*", "{\\a}", "{\\b}", "\\a", "\\b",
                      "[1]", "[2]", "[x]", "\\def", "\\def\\c#1#2", "\\c", "\\NewDocumentCommand",
                      "{s m}", "{o m}", "{O{d} m}", "\\IfValueTF{#1}", "\\IfBooleanTF{#1}",
                      "\\IfNoValueF{#2}", "\\providecommand", "\\newcommand\\B\\mathbb", "\\B"],
}
VOCAB["collect_macros_stripped"] = VOCAB["expand_macros"]


def soups(name: str, k: int, rng: random.Random) -> list[str]:
    vocab = VOCAB.get(name, []) * 3 + COMMON
    return ["".join(rng.choice(vocab) for _ in range(rng.randint(1, 40))) for _ in range(k)]


def main() -> None:
    # cap memory so a runaway expansion raises MemoryError instead of OOM-killing
    resource.setrlimit(resource.RLIMIT_AS, (4 << 30, 4 << 30))
    files = sorted(
        f for f in glob.glob("tests/**/*.tex", recursive=True)
        + glob.glob("tests/**/*.sty", recursive=True)
    )
    inputs: list[str] = []
    seen: set[str] = set()
    for f in files:
        with open(f, encoding="utf-8", errors="replace") as fh:
            text = fh.read()
        for c in chunks(text):
            if c not in seen and len(c) < 200_000:
                seen.add(c)
                inputs.append(c)
    # plus every string source recorded in the API goldens
    cases = os.path.join(os.path.dirname(OUT), "cases")
    for cid in sorted(os.listdir(cases)):
        with open(os.path.join(cases, cid, "case.json"), encoding="utf-8") as fh:
            src = json.load(fh).get("args", {}).get("source")
        if isinstance(src, str) and src not in seen:
            seen.add(src)
            inputs.append(src)
    rng = random.Random(0)
    rows = 0
    with open(OUT, "w", encoding="utf-8") as fh:
        for name, fn in FUNCS.items():
            kept_noop = kept = 0
            for s in inputs + soups(name, 400, random.Random(name)):
                out = guarded(fn, s)
                if out is None:
                    print(f"  {name}: skipped a {len(s)}-char input (blows up)", file=sys.stderr)
                    continue
                if out == s:
                    if kept_noop > (kept + 4) // 3 or rng.random() < 0.5:
                        continue
                    kept_noop += 1
                kept += 1
                fh.write(json.dumps({"f": name, "in": s, "out": out}, ensure_ascii=False) + "\n")
                rows += 1
            print(f"{name}: {kept} rows ({kept_noop} no-op)", file=sys.stderr)
    print(f"{rows} rows from {len(inputs)} inputs, {len(files)} files", file=sys.stderr)


if __name__ == "__main__":
    main()

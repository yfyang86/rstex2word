"""pytest plugin: record the Python reference's real ``re`` calls as goldens.

    GOLDEN_OUT=/path/to/goldens python -m pytest -p capture_regex -q

Wraps ``re._compile`` before tex2word is imported, so every pattern tex2word
compiles (module-level constants and ``re.sub(pattern, ...)`` calls alike) is
a recording proxy. For each distinct ``(pattern, flags)`` up to ``PER_PATTERN``
distinct subjects (each at most ``MAX_SUBJECT`` chars) are kept, preferring a
mix of matching and non-matching inputs, with Python's result:

* ``search``/``match``/``fullmatch`` -> ``null`` or the match spans;
* ``finditer`` -> the list of match spans;
* ``findall`` -> the list of strings / tuples;
* ``sub``/``subn`` with a string template -> the result (and count);
  with a callable -> the match spans it was called with;
* ``split`` -> the list of pieces.

Spans are UTF-8 byte offsets (the Rust engine's unit). Output:
``$GOLDEN_OUT/regex.jsonl``, one call per line.
"""

from __future__ import annotations

import json
import os
import re
import sys

PER_PATTERN = 6
MAX_SUBJECT = 20_000
# Python flag bits -> the Rust engine's (I=1, S=2, M=4, X=8); re.U is implicit.
_FLAG_MAP = {re.I: 1, re.S: 2, re.M: 4, re.X: 8}

_records: dict[tuple, list] = {}
_seen_keys: set = set()


def _rust_flags(flags: int) -> int:
    return sum(v for k, v in _FLAG_MAP.items() if flags & k)


def _boff(s: str):
    offs = [0]
    n = 0
    for ch in s:
        n += len(ch.encode("utf-8"))
        offs.append(n)
    return offs


def _span(m, offs):
    groups = []
    for i in range(1, (m.re.groups or 0) + 1):
        a, b = m.span(i)
        groups.append(None if a < 0 else [offs[a], offs[b]])
    return {"span": [offs[m.start()], offs[m.end()]], "groups": groups}


def _keep(key, subject, rec):
    if not isinstance(subject, str) or len(subject) > MAX_SUBJECT:
        return
    sig = (key, rec["m"], subject, json.dumps(rec.get("a"), sort_keys=True))
    if sig in _seen_keys:
        return
    bucket = _records.setdefault(key, [])
    matched = rec.get("hit", False)
    if len(bucket) >= PER_PATTERN:
        # keep diversity: replace a same-kind record only if this one adds a kind
        kinds = {r.get("hit", False) for r in bucket}
        if matched in kinds:
            return
        bucket.pop(0)
    _seen_keys.add(sig)
    bucket.append(rec)


class RecPattern:
    """A recording proxy around a compiled ``re.Pattern``."""

    def __init__(self, real):
        self._real = real
        self.pattern = real.pattern
        self.flags = real.flags
        self.groups = real.groups
        self.groupindex = real.groupindex

    def _key(self):
        return (self.pattern, _rust_flags(self.flags))

    def _rec(self, method, subject, args, result_json, hit):
        if not isinstance(self.pattern, str):
            return
        _keep(self._key(), subject,
              {"p": self.pattern, "f": _rust_flags(self.flags), "m": method,
               "s": subject, "a": args, "r": result_json, "hit": hit})

    def _one(self, name, string, pos=0, endpos=None):
        real = getattr(self._real, name)
        m = real(string, pos) if endpos is None else real(string, pos, endpos)
        if isinstance(string, str) and endpos is None:
            self._rec(name, string, {"pos": pos} if pos else None,
                      None if m is None else _span(m, _boff(string)), m is not None)
        return m

    def search(self, string, pos=0, endpos=None):
        return self._one("search", string, pos, endpos)

    def match(self, string, pos=0, endpos=None):
        return self._one("match", string, pos, endpos)

    def fullmatch(self, string, pos=0, endpos=None):
        return self._one("fullmatch", string, pos, endpos)

    def finditer(self, string, pos=0, endpos=None):
        ms = list(self._real.finditer(string, pos) if endpos is None
                  else self._real.finditer(string, pos, endpos))
        if isinstance(string, str) and endpos is None:
            offs = _boff(string)
            self._rec("finditer", string, {"pos": pos} if pos else None,
                      [_span(m, offs) for m in ms], bool(ms))
        return iter(ms)

    def findall(self, string, pos=0, endpos=None):
        res = (self._real.findall(string, pos) if endpos is None
               else self._real.findall(string, pos, endpos))
        if isinstance(string, str) and endpos is None:
            self._rec("findall", string, {"pos": pos} if pos else None,
                      [list(x) if isinstance(x, tuple) else x for x in res], bool(res))
        return res

    def sub(self, repl, string, count=0):
        return self._subn(repl, string, count, "sub")[0]

    def subn(self, repl, string, count=0):
        return self._subn(repl, string, count, "subn")

    def _subn(self, repl, string, count, name):
        if callable(repl):
            spans = []
            offs = _boff(string) if isinstance(string, str) else None

            def wrapped(m):
                if offs is not None:
                    spans.append(_span(m, offs))
                return repl(m)

            out = self._real.subn(wrapped, string, count)
            if offs is not None:
                self._rec("sub_callable", string, {"count": count} if count else None,
                          spans, bool(spans))
            return out
        out = self._real.subn(repl, string, count)
        if isinstance(string, str) and isinstance(repl, str):
            args = {"repl": repl}
            if count:
                args["count"] = count
            self._rec("subn", string, args, [out[0], out[1]], out[1] > 0)
        return out

    def split(self, string, maxsplit=0):
        res = self._real.split(string, maxsplit)
        if isinstance(string, str):
            self._rec("split", string, {"maxsplit": maxsplit} if maxsplit else None,
                      res, len(res) > 1)
        return res

    def scanner(self, *a, **k):
        return self._real.scanner(*a, **k)

    def __getattr__(self, name):
        return getattr(self._real, name)


def pytest_configure(config):
    if not os.environ.get("GOLDEN_OUT"):
        raise RuntimeError("set GOLDEN_OUT")
    real_compile = re._compile

    def rec_compile(pattern, flags):
        if isinstance(pattern, RecPattern):
            return pattern
        if isinstance(flags, re.RegexFlag):
            flags = flags.value
        return RecPattern(real_compile(pattern, flags))

    re._compile = rec_compile
    # tex2word must be imported *after* the patch so its module-level
    # re.compile() constants become recording proxies.
    for mod in [m for m in sys.modules if m.startswith("tex2word")]:
        del sys.modules[mod]


def pytest_unconfigure(config):
    out = os.path.join(os.environ["GOLDEN_OUT"], "regex.jsonl")
    n = 0
    with open(out, "w") as fh:
        for key in sorted(_records, key=lambda k: (k[0], k[1])):
            for rec in _records[key]:
                rec.pop("hit", None)
                fh.write(json.dumps(rec, ensure_ascii=False, sort_keys=True,
                                    separators=(",", ":")) + "\n")
                n += 1
    print(f"\n[golden] recorded {n} regex calls over {len(_records)} patterns -> {out}",
          file=sys.stderr)

"""pytest plugin: record golden cases from the Python tex2word reference.

Run the *Python* tex2word test suite with this plugin loaded and every call a
test makes to a captured public API is recorded together with Python's output:

    GOLDEN_OUT=/path/to/goldens \\
        python -m pytest -p capture_plugin -q   # from the Python repo root

Each unique call becomes a case directory under ``$GOLDEN_OUT/cases/<id>/``:

* ``case.json``  -- api name, arguments (strings inline, bytes/IR as refs),
                    and the pytest node ids that exercised it;
* ``files/``     -- a snapshot of the call's ``base_dir`` (and any file-path
                    argument), so ``\\input``/``\\includegraphics``/``.bib``
                    lookups resolve the same way when replayed;
* ``out.json``   -- Python's result: IR JSON (``Document.to_dict``), report
                    JSON, docx part references, or a plain return value.

Binary payloads (docx parts, media, input docx bytes) are stored once under
``$GOLDEN_OUT/blobs/<sha256>``. ``\\today`` is frozen so goldens are stable.

Only the *outermost* captured call is recorded (``convert_source`` calls
``parse_document`` internally; that inner call is not a separate case).
"""

from __future__ import annotations

import functools
import hashlib
import io
import json
import os
import shutil
import sys
import threading
import zipfile
from pathlib import Path

FROZEN_TODAY = "January 2, 2026"  # == SOURCE_DATE_EPOCH 1767312000 (2026-01-02 UTC)

# (module, function) pairs to record. Order doesn't matter.
CAPTURED = [
    ("tex2word.pipeline", "convert_source"),
    ("tex2word.frontend.parser", "parse_document"),
    ("tex2word.frontend.macros", "expand_macros"),
    ("tex2word.backend.latex_writer", "write_latex"),
    ("tex2word.frontend.docx_reader", "read_docx"),
    ("tex2word.roundtrip", "recover_ir"),
    ("tex2word.roundtrip", "to_latex"),
    ("tex2word.bib.bibtex", "parse_bibtex"),
    ("tex2word.mathml.omml", "latex_to_omml"),
    ("tex2word.validate", "validate_docx"),
    ("tex2word.transforms.crossref", "resolve_crossrefs"),
]

MAX_SNAPSHOT_BYTES = 8 * 1024 * 1024

_state = threading.local()
_out: Path | None = None
_repo_root = Path.cwd().resolve()
_seen: dict[str, dict] = {}


# --------------------------------------------------------------------------- #
# storage helpers
# --------------------------------------------------------------------------- #

def _blob(data: bytes) -> str:
    digest = hashlib.sha256(data).hexdigest()
    path = _out / "blobs" / digest
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    return digest


def _snapshot_dir(src: Path) -> dict[str, str] | None:
    """Snapshot a directory tree as {relpath: blob}; None if unsuitable."""
    try:
        src = src.resolve()
    except OSError:
        return None
    if not src.is_dir() or src == _repo_root or _repo_root.is_relative_to(src):
        return None
    files: dict[str, str] = {}
    total = 0
    for p in sorted(src.rglob("*")):
        if not p.is_file() or "__pycache__" in p.parts or p.suffix == ".docx" and p.stat().st_size > 2_000_000:
            continue
        size = p.stat().st_size
        total += size
        if total > MAX_SNAPSHOT_BYTES:
            return None
        files[str(p.relative_to(src))] = _blob(p.read_bytes())
    return files


def _ser(value, files: dict[str, str]):
    """Serialise an argument/return value into JSON-able form."""
    from tex2word import ir

    if value is None or isinstance(value, (bool, int, float, str)):
        if isinstance(value, str) and len(value) < 4096 and os.path.isfile(value):
            p = Path(value)
            files[f"@arg/{p.name}"] = _blob(p.read_bytes())
            return {"@file": f"@arg/{p.name}"}
        return value
    if isinstance(value, (bytes, bytearray)):
        return {"@blob": _blob(bytes(value))}
    if isinstance(value, ir.Document):
        return {"@ir": value.to_dict()}
    if isinstance(value, ir.Node):
        from tex2word.ir import _to_jsonable
        return {"@node": _to_jsonable(value)}
    if isinstance(value, dict):
        return {str(k): _ser(v, files) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_ser(v, files) for v in value]
    try:  # lxml element
        from lxml import etree
        if isinstance(value, etree._Element):
            return {"@xml": etree.tostring(value, encoding="unicode")}
    except ImportError:
        pass
    # ConversionReport and friends
    if hasattr(value, "to_json"):
        return {"@report": json.loads(value.to_json())}
    return {"@repr": repr(value)}


def _docx_parts(docx: bytes) -> dict:
    with zipfile.ZipFile(io.BytesIO(docx)) as zf:
        names = zf.namelist()
        return {"order": names, "parts": {n: _blob(zf.read(n)) for n in names}}


def _ser_result(api: str, result, files):
    if api == "convert_source":
        return {
            "ir": result.document.to_dict(),
            "report": json.loads(result.report.to_json()),
            "docx": _docx_parts(result.docx),
        }
    if api == "parse_document":
        doc, report = result
        return {"ir": doc.to_dict(), "report": json.loads(report.to_json())}
    if api == "resolve_crossrefs":
        return {"ir": result.to_dict()}
    return {"value": _ser(result, files)}


# --------------------------------------------------------------------------- #
# wrapping
# --------------------------------------------------------------------------- #

def _wrap(api: str, fn):
    import inspect

    sig = inspect.signature(fn)

    @functools.wraps(fn)
    def wrapper(*args, **kwargs):
        depth = getattr(_state, "depth", 0)
        if depth > 0:
            return fn(*args, **kwargs)
        _state.depth = depth + 1
        try:
            bound = sig.bind(*args, **kwargs)
            # Serialise inputs *before* the call: some APIs mutate their input.
            files: dict[str, str] = {}
            call_args = {}
            base_dir = bound.arguments.get("base_dir")
            for name, value in bound.arguments.items():
                if name == "base_dir":
                    continue
                if name == "report":
                    call_args[name] = {"@report": json.loads(value.to_json())}
                    continue
                call_args[name] = _ser(value, files)
            snap = None
            if isinstance(base_dir, str) and base_dir not in (".", ""):
                snap = _snapshot_dir(Path(base_dir))
            if snap:
                files.update({f"base/{k}": v for k, v in snap.items()})
            # Absolute paths in outputs (report messages, …) are run-specific;
            # record them as placeholders the Rust replay substitutes back.
            subst: list[tuple[str, str]] = []
            for name, value in bound.arguments.items():
                if isinstance(value, str) and len(value) < 4096 and os.path.isfile(value):
                    p = Path(value)
                    subst += [(str(p.resolve()), f"{{ARG:{p.name}}}"), (value, f"{{ARG:{p.name}}}")]
            if snap and isinstance(base_dir, str):
                subst += [(str(Path(base_dir).resolve()), "{BASE}"), (base_dir, "{BASE}")]
            subst.sort(key=lambda kv: -len(kv[0]))  # longest first
            outcome: dict
            try:
                result = fn(*args, **kwargs)
                outcome = _subst(_ser_result(api, result, files), subst)
            except Exception as exc:  # record failures too: Rust must match
                outcome = _subst({"raises": type(exc).__name__, "message": str(exc)}, subst)
                _record(api, call_args, base_dir, snap is not None, files, outcome)
                raise
            _record(api, call_args, base_dir, snap is not None, files, outcome)
            return result
        finally:
            _state.depth = depth

    wrapper.__golden_original__ = fn
    return wrapper


def _subst(obj, subst: list[tuple[str, str]]):
    """Replace run-specific path strings inside a JSON-able value."""
    if not subst:
        return obj
    if isinstance(obj, str):
        for old, new in subst:
            if old and old in obj:
                obj = obj.replace(old, new)
        return obj
    if isinstance(obj, dict):
        return {k: _subst(v, subst) for k, v in obj.items()}
    if isinstance(obj, list):
        return [_subst(v, subst) for v in obj]
    return obj


def _record(api, call_args, base_dir, has_base, files, outcome):
    case = {
        "api": api,
        "args": call_args,
        "base_dir": "base" if has_base else (base_dir if base_dir not in (None,) else "."),
        "files": dict(sorted(files.items())),
    }
    key = json.dumps(case, sort_keys=True, ensure_ascii=False)
    cid = hashlib.sha256(key.encode()).hexdigest()[:16]
    test = os.environ.get("PYTEST_CURRENT_TEST", "?").rsplit(" (", 1)[0]
    entry = _seen.get(cid)
    if entry is None:
        entry = {"case": case, "tests": [], "out": outcome}
        _seen[cid] = entry
    if test not in entry["tests"]:
        entry["tests"].append(test)


def pytest_configure(config):
    global _out
    out = os.environ.get("GOLDEN_OUT")
    if not out:
        raise RuntimeError("set GOLDEN_OUT to the goldens output directory")
    _out = Path(out).resolve()
    if (_out / "cases").exists():
        shutil.rmtree(_out / "cases")
    (_out / "cases").mkdir(parents=True, exist_ok=True)

    import importlib
    import time as _time
    import types

    # Tests assemble their own .docx inputs with zipfile.writestr/write, which
    # stamp members with the wall clock. Pin zipfile's clock so those inputs
    # (and so the case ids derived from them) are byte-stable across runs.
    shim = types.SimpleNamespace(
        **{k: getattr(_time, k) for k in dir(_time) if not k.startswith("__")})
    shim.localtime = lambda *a: _time.struct_time((1980, 1, 1, 0, 0, 0, 1, 1, -1))
    zipfile.time = shim

    parser = importlib.import_module("tex2word.frontend.parser")
    parser._today_text = lambda: FROZEN_TODAY

    originals = {}
    for mod_name, fn_name in CAPTURED:
        mod = importlib.import_module(mod_name)
        fn = getattr(mod, fn_name)
        originals[id(fn)] = (fn, _wrap(fn_name, fn))
    # Rebind every module-level reference to an original (catches re-exports
    # and `from x import f` bindings already made inside tex2word).
    for mod in list(sys.modules.values()):
        if mod is None or not getattr(mod, "__name__", "").startswith("tex2word"):
            continue
        for attr, val in list(vars(mod).items()):
            hit = originals.get(id(val))
            if hit is not None and val is hit[0]:
                setattr(mod, attr, hit[1])


def pytest_unconfigure(config):
    if _out is None:
        return
    index = {}
    for cid, entry in sorted(_seen.items()):
        d = _out / "cases" / cid
        d.mkdir(parents=True, exist_ok=True)
        (d / "case.json").write_text(
            json.dumps({**entry["case"], "tests": sorted(entry["tests"])},
                       indent=0, ensure_ascii=False, sort_keys=True) + "\n")
        (d / "out.json").write_text(
            json.dumps(entry["out"], ensure_ascii=False, sort_keys=True,
                       separators=(",", ":")) + "\n")
        index[cid] = entry["case"]["api"]
    (_out / "index.json").write_text(json.dumps(index, indent=1, sort_keys=True) + "\n")
    _gc_blobs()
    print(f"\n[golden] recorded {len(_seen)} cases -> {_out}", file=sys.stderr)


def _gc_blobs() -> None:
    """Delete blobs no recorded case references (e.g. abandoned snapshots)."""
    live: set[str] = set()

    def walk(o):
        if isinstance(o, dict):
            if "@blob" in o:
                live.add(o["@blob"])
            for v in o.values():
                walk(v)
        elif isinstance(o, list):
            for v in o:
                walk(v)

    for entry in _seen.values():
        live.update(entry["case"]["files"].values())
        walk(entry["case"]["args"])
        out = entry["out"]
        live.update(out.get("docx", {}).get("parts", {}).values())
        walk(out.get("value"))
    blobs = _out / "blobs"
    for p in blobs.iterdir():
        if p.name not in live:
            p.unlink()

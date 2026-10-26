"""Record the reverse path on every recorded conversion.

For each ``convert_source`` case, rebuilds the ``.docx`` from its recorded
parts and records sha256 digests of Python's ``read_docx`` IR (canonical
JSON), ``to_latex`` (reconcile on/off) and ``write_latex(read_docx(...))``
(the foreign-document path) to ``goldens/reverse.jsonl``. The Rust test runs
its own (byte-identical) conversion and compares.

    /home/user/py-core/bin/python tools/golden/gen_reverse_goldens.py
"""

import hashlib
import io
import json
import os
import zipfile

from tex2word.backend.latex_writer import write_latex
from tex2word.frontend.docx_reader import read_docx
from tex2word.roundtrip import to_latex

GOLDENS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                       "crates", "tex2word-goldens", "goldens")


def sha(text: str | None) -> str | None:
    return None if text is None else hashlib.sha256(text.encode("utf-8")).hexdigest()


def canon(doc) -> str:
    return json.dumps(doc.to_dict(), sort_keys=True, ensure_ascii=False)


INSERTED = b"<w:p><w:r><w:t>Inserted paragraph.</w:t></w:r></w:p>"


def edits(xml: bytes) -> dict[str, bytes]:
    """Deterministic Word-side edits of document.xml (mirrored by the Rust test)."""
    out = {"prose": xml.replace(b" the ", b" teh ")}
    start = xml.find(b"<w:p>")
    end = xml.find(b"</w:p>", start)
    if start >= 0 and end >= 0:
        out["delete"] = xml[:start] + xml[end + 6:]
        out["insert"] = xml[:end + 6] + INSERTED + xml[end + 6:]
    return out


def build(order, parts) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        for name in order:
            zf.writestr(name, parts[name])
    return buf.getvalue()


def package_zip(order, parts) -> bytes:
    """The archive exactly as tex2word's DocxPackage writes it."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        for name in sorted(order):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(info, parts[name])
    return buf.getvalue()


def attempt(fn):
    try:
        return fn()
    except Exception as exc:  # recorded as the exception type
        return {"error": type(exc).__name__}


def main() -> None:
    rows = []
    cases = os.path.join(GOLDENS, "cases")
    for cid in sorted(os.listdir(cases)):
        case = json.load(open(os.path.join(cases, cid, "case.json")))
        if case["api"] != "convert_source":
            continue
        out = json.load(open(os.path.join(cases, cid, "out.json")))
        docx = out["docx"]
        parts = {n: open(os.path.join(GOLDENS, "blobs", docx["parts"][n]), "rb").read()
                 for n in docx["order"]}
        data = build(docx["order"], parts)
        read = attempt(lambda: read_docx(data))
        row = {
            "case": cid,
            "docx": hashlib.sha256(package_zip(docx["order"], parts)).hexdigest(),
            "read": read if isinstance(read, dict) else sha(canon(read)),
            "latex": attempt(lambda: sha(to_latex(data))),
            "latex_raw": attempt(lambda: sha(to_latex(data, reconcile=False))),
            "foreign": read if isinstance(read, dict) else attempt(lambda: sha(write_latex(read))),
        }
        for kind, xml in edits(parts["word/document.xml"]).items():
            edited = build(docx["order"], {**parts, "word/document.xml": xml})
            row[f"edit_{kind}"] = attempt(lambda: sha(to_latex(edited)))
        rows.append(row)
    with open(os.path.join(GOLDENS, "reverse.jsonl"), "w") as fh:
        for r in rows:
            fh.write(json.dumps(r, sort_keys=True) + "\n")
    print(len(rows), "rows")


if __name__ == "__main__":
    main()

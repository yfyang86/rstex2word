"""Record lxml's parse + re-serialize of every XML blob in the goldens.

For each blob that lxml parses, writes ``{"sha": <input blob>, "out":
sha256(tostring(fromstring(blob), xml_declaration=True, encoding="UTF-8",
standalone=True)), "ascii": sha256(tostring(root))}`` (or ``"error": true``)
to ``goldens/xml.jsonl`` -- a differential test of the Rust parser and
serializer against libxml2.

    /home/user/py-core/bin/python tools/golden/gen_xml_goldens.py
"""

import hashlib
import json
import os

from lxml import etree

GOLDENS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                       "crates", "tex2word-goldens", "goldens")


def main() -> None:
    rows = []
    blobs = os.path.join(GOLDENS, "blobs")
    for sha in sorted(os.listdir(blobs)):
        data = open(os.path.join(blobs, sha), "rb").read()
        head = data.lstrip()[:5]
        if not head.startswith(b"<"):
            continue
        try:
            root = etree.fromstring(data)
        except Exception:
            rows.append({"sha": sha, "error": True})
            continue
        out = etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)
        asc = etree.tostring(root)
        rows.append({"sha": sha, "out": hashlib.sha256(out).hexdigest(),
                     "ascii": hashlib.sha256(asc).hexdigest()})
    with open(os.path.join(GOLDENS, "xml.jsonl"), "w") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    print(len(rows), "xml goldens")


if __name__ == "__main__":
    main()

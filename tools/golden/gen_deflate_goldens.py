"""Record zlib's raw DEFLATE (level 6, as zipfile uses it) of every blob.

Writes ``{"sha": <blob>, "out": sha256(compressed)}`` rows to
``goldens/deflate.jsonl`` -- a differential test of the Rust zlib port over
text, XML and binary inputs of every size in the goldens.

    /home/user/py-core/bin/python tools/golden/gen_deflate_goldens.py
"""

import hashlib
import json
import os
import zlib

GOLDENS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                       "crates", "tex2word-goldens", "goldens")


def main() -> None:
    rows = []
    blobs = os.path.join(GOLDENS, "blobs")
    for sha in sorted(os.listdir(blobs)):
        data = open(os.path.join(blobs, sha), "rb").read()
        c = zlib.compressobj(zlib.Z_DEFAULT_COMPRESSION, zlib.DEFLATED, -15)
        out = c.compress(data) + c.flush()
        rows.append({"sha": sha, "out": hashlib.sha256(out).hexdigest()})
    with open(os.path.join(GOLDENS, "deflate.jsonl"), "w") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    print(len(rows), "rows")


if __name__ == "__main__":
    main()

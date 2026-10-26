"""Record lxml's error message for malformed XML.

Writes ``{"in": <bytes as latin-1 text>, "err": str(XMLSyntaxError) | null}``
rows to ``goldens/xmlerr.jsonl``: hand-written cases plus deterministic
mutations (truncations, broken tags, bad references, bad characters, dropped
namespace declarations) of small XML parts from the goldens. A differential
test of the Rust parser's libxml2-style messages and positions.

    /home/user/py-core/bin/python tools/golden/gen_xmlerr_goldens.py
"""

import json
import os
import random
import re

from lxml import etree

GOLDENS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                       "crates", "tex2word-goldens", "goldens")

HAND = [
    b'<w:document><unclosed>', b'', b'   ', b'abc', b'<a>', b'<a><b></a>', b'<a></b>', b'<a/><b/>',
    b'<a/>junk', b'<a x="1" x="2"/>', b'<a x=1/>', b'<a x/>', b'<a>&foo;</a>', b'<a>&#0;</a>',
    b'<a><!-- x</a>', b'<a><![CDATA[x</a>', b'<a>\n<b>\n</c>\n</a>', b'<?xml version="1.0"?>\n<a>\n<b></a>',
    b'<a b:c="1"/>', b'<x:a xmlns:y="u"/>', b'<a>\xff</a>', b'<a>\x01</a>', b'<a', b'<a b="1"', b'<a b="1',
    b'<a>< b/></a>', b'<1a/>', b'<a></a', b'<w:document xmlns:w="u"><w:body><x:p/></w:body></w:document>',
    b'<a>]]></a>', b'<a>&amp</a>', b'<a>text', b'<?xml version="1.0"?>', b'<!-- c -->', b'<a/><!-- c',
    b'\n\n<a>\n  <b>\n</a>', b'<a><b x="<"/></a>', b'<a>&#xZZ;</a>', b'<a b="&lt;" b="1"/>',
    b'<?xml version="1.0" encoding="UTF-8"?><Types xmlns="x"><Default', b'<a><b><c>', b'<a>x<b>y',
    b'<a b="\x01"/>', b'<a><!-- \x01 --></a>', b'<a>&#x110000;</a>', b'<a>&#xD800;</a>', b'<a>&lt</a>',
    b'<a>& x</a>', b'<a>&#;</a>', b'<a>&#12a;</a>', b'<a x="1"y="2"/>', b'<a>\xc3</a>', b'<a>\xc3\xa9\xff</a>',
    b'\xef\xbb\xbf<a/>', b'<a></a >', b'<a></ a>', b'<a><?pi x</a>', b'<a><?xml x?></a>', b'<a/>\n<!-- ok -->\n',
    b'<a/>\n<?pi?>', b'<!DOCTYPE a><a/>', b'<a xmlns:p=""/>', b'<p:a xmlns:p=""/>', b'<a xmlns:xml="x"/>',
    b'<a b="1" xmlns:b="u" b:c="1" b:c="2"/>', b'<a:b:c/>', b'<a x="&foo;"/>', b'<a x="&#0;"/>',
    b'<a>\r\n<b>\r\n</a>', b'<a>\xe2\x82</a>', b'<a>\x00</a>', b'<a\n  x="1"\n  x="2"/>',
]


def mutations(data: bytes, rng: random.Random) -> list[bytes]:
    out = []
    n = len(data)
    for _ in range(4):
        out.append(data[: rng.randrange(1, n)])
    gts = [m.start() for m in re.finditer(rb">", data)]
    if gts:
        k = rng.choice(gts)
        out.append(data[:k] + data[k + 1:])
    ends = [m for m in re.finditer(rb"</([^>]+)>", data)]
    if ends:
        m = rng.choice(ends)
        out.append(data[: m.start(1)] + b"zz" + data[m.start(1):])
    texts = [m.start() for m in re.finditer(rb">[^<]", data)]
    for ins in (b"&x;", b"<", b"&", b"\x01", b"\xff", b"]]>", b"&#0;"):
        if texts:
            k = rng.choice(texts) + 1
            out.append(data[:k] + ins + data[k:])
    decls = [m for m in re.finditer(rb' xmlns:\w+="[^"]*"', data)]
    if decls:
        m = rng.choice(decls)
        out.append(data[: m.start()] + data[m.end():])
    attrs = [m for m in re.finditer(rb' [\w:]+="[^"]*"', data)]
    if attrs:
        m = rng.choice(attrs)
        out.append(data[: m.end()] + m.group(0) + data[m.end():])
    return out


def main() -> None:
    rng = random.Random(20260102)
    inputs = list(HAND)
    blobs = os.path.join(GOLDENS, "blobs")
    sources = []
    for sha in sorted(os.listdir(blobs)):
        data = open(os.path.join(blobs, sha), "rb").read()
        if not data.lstrip().startswith(b"<") or len(data) > 1500:
            continue
        try:
            etree.fromstring(data)
        except Exception:
            continue
        sources.append(data)
    for data in rng.sample(sources, min(len(sources), 150)):
        inputs += mutations(data, rng)
    rows, seen = [], set()
    for data in inputs:
        if data in seen:
            continue
        seen.add(data)
        try:
            etree.fromstring(data)
            err = None
        except etree.XMLSyntaxError as exc:
            err = str(exc)
        rows.append({"in": data.decode("latin-1"), "err": err})
    with open(os.path.join(GOLDENS, "xmlerr.jsonl"), "w") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=True) + "\n")
    print(len(rows), "rows,", sum(r["err"] is not None for r in rows), "errors")


if __name__ == "__main__":
    main()

"""List Python source lines holding invisible non-ASCII characters (spaces,
format chars, combining marks) -- the ones that are easy to mistype when
porting string literals. Usage: python scan_invisible.py FILE.py ...
"""

import sys
import unicodedata

for f in sys.argv[1:]:
    for ln, line in enumerate(open(f, encoding="utf-8"), 1):
        hits = sorted({hex(ord(c)) for c in line if ord(c) > 127 and (
            c.isspace() or unicodedata.category(c) in ("Cf", "Zs", "Mn", "Cc"))})
        if hits:
            print(f"{f}:{ln}: {' '.join(hits)}: {line.strip()[:100]}")

"""Record the report helpers on every recorded conversion report.

Rebuilds Python's ``ConversionReport`` from each ``convert_source`` case's
recorded report and writes sha256 digests of ``to_json()``, ``summary()`` and
``to_html()`` -- plus ``aggregate_html`` over all of them (named by case id)
-- to ``goldens/report.jsonl``.

    /home/user/py-core/bin/python tools/golden/gen_report_goldens.py
"""

import hashlib
import json
import os

from tex2word.report import ConversionReport, ReportEntry, aggregate_html

GOLDENS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                       "crates", "tex2word-goldens", "goldens")


def sha(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def main() -> None:
    rows, reports = [], []
    cases = os.path.join(GOLDENS, "cases")
    for cid in sorted(os.listdir(cases)):
        case = json.load(open(os.path.join(cases, cid, "case.json")))
        if case["api"] != "convert_source":
            continue
        rec = json.load(open(os.path.join(cases, cid, "out.json")))["report"]
        rep = ConversionReport(
            entries=[ReportEntry(e["severity"], e["construct"], e["message"]) for e in rec["entries"]],
            math_omml=rec["coverage"]["math_omml"],
            math_image=rec["coverage"]["math_image"],
            math_raw=rec["coverage"]["math_raw"],
        )
        reports.append((cid, rep))
        rows.append({"case": cid, "json": sha(rep.to_json()), "summary": rep.summary(),
                     "html": sha(rep.to_html())})
    rows.append({"aggregate": sha(aggregate_html(reports))})
    with open(os.path.join(GOLDENS, "report.jsonl"), "w") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(len(rows), "rows")


if __name__ == "__main__":
    main()

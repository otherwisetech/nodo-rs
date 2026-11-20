#!/usr/bin/env python3
"""Rebuild crates/core/data/oui.tsv.gz from the IEEE MA-L registry.

Usage: curl -sSLo /tmp/oui.csv https://standards-oui.ieee.org/oui/oui.csv
       python3 script/update-oui.py /tmp/oui.csv
One line per prefix, sorted: `AABBCC<TAB>Organization`. Nodo embeds the gzip and looks
prefixes up offline; nothing is fetched at runtime.
"""
import csv, gzip, sys, pathlib

src = sys.argv[1]
rows = {}
with open(src, newline="", encoding="utf-8") as f:
    for r in csv.DictReader(f):
        if r["Registry"] != "MA-L":
            continue
        name = " ".join(r["Organization Name"].replace("\t", " ").split())
        if name:
            rows[r["Assignment"].strip().upper()] = name
out = pathlib.Path(__file__).resolve().parent.parent / "crates/core/data/oui.tsv.gz"
data = "".join(f"{k}\t{v}\n" for k, v in sorted(rows.items())).encode()
with open(out, "wb") as f:
    f.write(gzip.compress(data, compresslevel=9, mtime=0))
print(f"{len(rows)} prefixes -> {out} ({out.stat().st_size} bytes)")

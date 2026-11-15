"""Regenerate THIRD_PARTY_NOTICES.md from the crates compiled into Berth.exe.

Run from the repository root after `cargo fetch`:
    python tools/gen_notices.py
"""
import glob
import json
import os
import subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CARGO = os.environ.get("CARGO", "cargo")

meta = json.loads(subprocess.check_output(
    [CARGO, "metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"],
    cwd=ROOT))
tree = subprocess.check_output(
    [CARGO, "tree", "-e", "normal", "--target", "x86_64-pc-windows-msvc", "--prefix", "none", "--format", "{p}"],
    cwd=ROOT, text=True)
used = set()
for line in tree.splitlines():
    parts = line.split()
    if len(parts) >= 2 and parts[0] != "berth":
        used.add((parts[0], parts[1].lstrip("v")))

pkgs = sorted((p for p in meta["packages"] if (p["name"], p["version"]) in used),
              key=lambda p: (p["name"], p["version"]))
out = [
    "# Third-party notices",
    "",
    "Berth is distributed under the MIT License (see `LICENSE`). Berth.exe also contains code",
    "from the Rust standard library (MIT OR Apache-2.0) and the open-source crates listed below.",
    "Each crate's license and copyright notices are reproduced in full.",
    "",
    "Berth does not bundle any fonts, images or icons from third parties: app icons shown on",
    "the dock are read from Windows at run time, text uses the Segoe UI fonts installed with",
    "Windows, and Berth's own icon was drawn for this project (`tools/make_icon.py`).",
    "",
    "| Crate | Version | License | Source |",
    "|---|---|---|---|",
]
for p in pkgs:
    out.append(f"| {p['name']} | {p['version']} | {p.get('license') or 'see below'} | {p.get('repository') or ''} |")
out.append("")
seen = {}
for p in pkgs:
    d = os.path.dirname(p["manifest_path"])
    files = sorted(f for f in glob.glob(os.path.join(d, "*"))
                   if os.path.basename(f).upper().startswith(("LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE")))
    out.append(f"## {p['name']} {p['version']}")
    out.append("")
    if not files:
        out.append(f"License: {p.get('license')}. No license file is shipped in the crate package; "
                   f"see {p.get('repository') or 'the crate source'}.")
        out.append("")
    for f in files:
        text = open(f, encoding="utf-8", errors="replace").read().strip()
        key = text
        if key in seen:
            out.append(f"`{os.path.basename(f)}`: identical to the text reproduced for {seen[key]}.")
            out.append("")
            continue
        seen[key] = f"{p['name']} {p['version']} ({os.path.basename(f)})"
        out.append(f"<details><summary>{os.path.basename(f)}</summary>")
        out.append("")
        out.append("```text")
        out.append(text)
        out.append("```")
        out.append("")
        out.append("</details>")
        out.append("")
open(os.path.join(ROOT, "THIRD_PARTY_NOTICES.md"), "w", encoding="utf-8").write("\n".join(out) + "\n")
print(f"wrote notices for {len(pkgs)} crates")

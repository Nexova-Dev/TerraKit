#!/usr/bin/env python3
"""
Internal link checker.

A spec that is read rather than run still has to be correct, and a broken
cross-reference is a broken cross-reference: it sends a contributor to a file
that does not exist and they conclude the design was never decided.

Checks every relative markdown link resolves, and that every ADR referenced by
number actually exists.

Run:  python3 tools/check_links.py
"""

from __future__ import annotations

import pathlib
import re
import sys

LINK_RE = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
ADR_RE = re.compile(r"ADR-(\d{3})")
EXTERNAL_PREFIXES = ("http://", "https://", "mailto:", "#")


def main() -> int:
    root = pathlib.Path(__file__).resolve().parents[1]
    markdown = sorted(
        path
        for path in root.rglob("*.md")
        if ".git" not in path.parts and "node_modules" not in path.parts
    )

    checked = 0
    problems: list[str] = []

    adr_files = {
        path.name.split("-")[1]
        for path in (root / "docs" / "adr").glob("ADR-*.md")
    }

    for path in markdown:
        text = path.read_text(encoding="utf-8")
        relative = path.relative_to(root)

        for label, target in LINK_RE.findall(text):
            if target.startswith(EXTERNAL_PREFIXES):
                continue
            checked += 1
            clean = target.split("#", 1)[0]
            if not clean:
                continue
            if not (path.parent / clean).resolve().exists():
                problems.append(f"{relative}: [{label}]({target}) does not exist")

        # An ADR cited by number must exist, or the citation is a fiction.
        for number in set(ADR_RE.findall(text)):
            if number not in adr_files:
                problems.append(
                    f"{relative}: cites ADR-{number}, which is not in docs/adr/"
                )

    print(f"checked {checked} internal links across {len(markdown)} markdown files")

    if problems:
        print(f"\n{len(problems)} problem(s):", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1

    print(f"all internal links resolve; {len(adr_files)} ADRs present")
    return 0


if __name__ == "__main__":
    sys.exit(main())

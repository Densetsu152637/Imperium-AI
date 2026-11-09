#!/usr/bin/env python3
"""Check relative links in the repository's Markdown files, offline.

Every inline link or reference definition that is not a URL must name a file or folder that
exists, and a `#fragment` into a Markdown file must match one of its headings (GitHub's anchor
rules). Code blocks and inline code are ignored. Usage: check-links.py [root]
"""

import os
import re
import sys
from pathlib import Path

SKIP_DIRS = {".git", "node_modules", "target", "policy", "dist"}
LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
REFERENCE = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s+.*)?$", re.MULTILINE)
INLINE_CODE = re.compile(r"`[^`\n]*`")
HEADING = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$", re.MULTILINE)
EXTERNAL = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")


def strip_code(text: str, inline: bool = True) -> str:
    """Drops fenced code blocks (``` or ~~~) and, unless `inline` is false, inline code."""
    kept = []
    fence = None
    for line in text.splitlines():
        marker = line.lstrip()[:3]
        if fence is None and marker in ("```", "~~~"):
            fence = marker
        elif fence is not None and marker == fence:
            fence = None
        elif fence is None:
            kept.append(line)
    kept_text = "\n".join(kept)
    return INLINE_CODE.sub("", kept_text) if inline else kept_text


def markdown_files(root: Path) -> list[Path]:
    found = []
    for folder, dirs, files in os.walk(root):
        dirs[:] = [name for name in dirs if name not in SKIP_DIRS]
        found.extend(Path(folder) / name for name in files if name.endswith(".md"))
    return sorted(found)


def anchors(path: Path, cache: dict[Path, set[str]]) -> set[str]:
    """GitHub's heading anchors: lower case, punctuation dropped, spaces to hyphens, repeats
    numbered."""
    if path not in cache:
        seen: dict[str, int] = {}
        found: set[str] = set()
        # Inline code in a heading keeps its text in the anchor (backticks are punctuation).
        text = strip_code(path.read_text(encoding="utf-8"), inline=False)
        for heading in HEADING.findall(text):
            text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)  # links keep their text
            text = re.sub(r"<[^>]+>", "", text).strip().lower()
            slug = re.sub(r"[^\w\- ]", "", text).replace(" ", "-")
            count = seen.get(slug, 0)
            seen[slug] = count + 1
            found.add(slug if count == 0 else f"{slug}-{count}")
        cache[path] = found
    return cache[path]


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    cache: dict[Path, set[str]] = {}
    problems = []
    files = markdown_files(root)
    for path in files:
        text = strip_code(path.read_text(encoding="utf-8"))
        targets = LINK.findall(text) + REFERENCE.findall(text)
        for target in targets:
            if EXTERNAL.match(target) or target.startswith("//"):
                continue
            file_part, _, fragment = target.partition("#")
            destination = path if not file_part else (path.parent / file_part).resolve()
            where = f"{path.relative_to(root)}: {target}"
            if file_part.startswith("/"):
                destination = (root / file_part.lstrip("/")).resolve()
            if not destination.exists():
                problems.append(f"{where}: no such file")
            elif fragment and destination.suffix == ".md" and fragment.lower() not in anchors(destination, cache):
                problems.append(f"{where}: no heading #{fragment}")
    for problem in problems:
        print(problem)
    print(f"Checked {len(files)} Markdown files: {len(problems)} broken links.")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

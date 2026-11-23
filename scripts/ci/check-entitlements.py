#!/usr/bin/env python3
"""Fails when a macOS entitlements file grants what would let code be injected into the app.

The daemon counts a plugin approval only from the installed desktop app's native dialog, and that
rests on the hardened runtime: dyld ignores DYLD_* variables, library validation refuses unsigned
libraries, and nothing can attach to the app's WebKit inspector. Any of these keys, set to any
value, undoes that, so the check fails on presence rather than on a true value.

Usage:
  check-entitlements.py <file.plist>...        check these files
  check-entitlements.py --config <tauri.conf>  check the file bundle.macOS.entitlements names
                                               (none named: nothing is granted, which passes)
"""
import json
import plistlib
import sys
from pathlib import Path

FORBIDDEN = {
    "com.apple.security.cs.allow-dyld-environment-variables": "lets DYLD_* variables inject libraries",
    "com.apple.security.cs.disable-library-validation": "lets the app load libraries signed by anyone",
    "com.apple.security.get-task-allow": "lets a debugger attach, which includes WebKit's inspector",
    "com.apple.security.cs.debugger": "lets the app debug other processes",
}


def check(path: Path) -> list[str]:
    try:
        with path.open("rb") as handle:
            entitlements = plistlib.load(handle)
    except (OSError, plistlib.InvalidFileException, ValueError) as error:
        return [f"{path}: cannot be read as a property list: {error}"]
    if not isinstance(entitlements, dict):
        return [f"{path}: is not a dictionary of entitlements"]
    return [f"{path}: {key} {why}" for key, why in FORBIDDEN.items() if key in entitlements]


def configured(config: Path) -> list[Path]:
    data = json.loads(config.read_text(encoding="utf-8"))
    named = (data.get("bundle") or {}).get("macOS", {}).get("entitlements")
    return [config.parent / named] if named else []


def main(argv: list[str]) -> int:
    if len(argv) == 2 and argv[0] == "--config":
        files = configured(Path(argv[1]))
        if not files:
            print("no entitlements are configured: none are granted")
    elif argv and not argv[0].startswith("-"):
        files = [Path(name) for name in argv]
    else:
        print(__doc__, file=sys.stderr)
        return 2
    problems = []
    for file in files:
        problems += check(file)
        print(f"checked {file}")
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

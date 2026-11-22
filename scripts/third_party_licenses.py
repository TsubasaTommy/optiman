#!/usr/bin/env python3
"""Writes THIRD_PARTY_LICENSES.txt: the license texts of everything linked into the release
binaries (Rust crates for the macOS and Windows targets, and the bundled libjxl sources).

Usage: python3 scripts/third_party_licenses.py [--check]
  --check  exit 1 if the committed file is out of date (used by CI)
"""

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "THIRD_PARTY_LICENSES.txt"
TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
]
LICENSE_PREFIXES = ("license", "licence", "copying", "notice", "unlicense")
NATIVE = [
    ("libjxl", "third_party/libjxl", ["LICENSE", "PATENTS"]),
    ("highway", "third_party/libjxl/third_party/highway", ["LICENSE", "LICENSE-BSD3"]),
    ("brotli", "third_party/libjxl/third_party/brotli", ["LICENSE"]),
    ("skcms", "third_party/libjxl/third_party/skcms", ["LICENSE"]),
]


def is_proc_macro(package):
    return any("proc-macro" in t["kind"] for t in package["targets"])


def shipped_packages():
    """Packages linked into the binaries: reachable through normal (non-dev, non-build) dependencies."""
    packages = {}
    for target in TARGETS:
        meta = json.loads(
            subprocess.check_output(
                ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],
                cwd=ROOT,
            )
        )
        by_id = {p["id"]: p for p in meta["packages"]}
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        root = meta["resolve"]["root"]
        stack, seen = [root], {root}
        while stack:
            for dep in nodes[stack.pop()]["deps"]:
                pkg = dep["pkg"]
                # Proc macros (and what only they use) run in the compiler, not in the binary.
                if is_proc_macro(by_id[pkg]):
                    continue
                if any(k["kind"] is None for k in dep["dep_kinds"]) and pkg not in seen:
                    seen.add(pkg)
                    stack.append(pkg)
        for pid in seen - {root}:
            p = by_id[pid]
            packages[(p["name"], p["version"])] = p
    return [packages[k] for k in sorted(packages)]


def license_files(directory):
    return sorted(
        f for f in directory.iterdir() if f.is_file() and f.name.lower().startswith(LICENSE_PREFIXES)
    )


def section(title, files, note=None):
    parts = ["=" * 78, title, "=" * 78]
    if note:
        parts.append(note)
    for f in files:
        text = f.read_text(encoding="utf-8", errors="replace").strip()
        parts += ["", f"--- {f.name} ---", "", text]
    return "\n".join(parts) + "\n\n"


def render():
    out = [
        "Third-party software in Optiman\n"
        "==============================================\n\n"
        "The release binaries statically link the following software. Their licenses follow.\n\n"
    ]
    for name, path, names in NATIVE:
        d = ROOT / path
        files = [d / n for n in names if (d / n).exists()]
        if not files:
            sys.exit(f"missing license files for {name} in {path} (initialize the submodules)")
        out.append(section(f"{name} (C/C++, bundled from {path})", files))
    for p in shipped_packages():
        d = pathlib.Path(p["manifest_path"]).parent
        files = license_files(d)
        note = f"License: {p.get('license') or 'see files'}"
        if p.get("repository"):
            note += f"\nRepository: {p['repository']}"
        if not files:
            note += "\n(no license file in the published crate; the license above applies)"
        out.append(section(f"{p['name']} {p['version']} (Rust crate)", files, note))
    return "".join(out)


def main():
    text = render()
    if "--check" in sys.argv:
        if not OUT.exists() or OUT.read_bytes() != text.encode("utf-8"):
            sys.exit("THIRD_PARTY_LICENSES.txt is out of date: run python3 scripts/third_party_licenses.py")
        return
    OUT.write_bytes(text.encode("utf-8"))
    print(f"wrote {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Renders the Homebrew formula and the winget manifests for a published release.

Usage: python3 scripts/render_packaging.py VERSION SHA256SUMS.txt OUT_DIR
Writes OUT_DIR/homebrew/optimize-image-folder.rb and
OUT_DIR/winget/manifests/t/TsubasaTommy/OptimizeImageFolder/VERSION/*.yaml
"""

import datetime
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def main():
    version, sums_path, out = sys.argv[1], pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3])
    sums = {}
    for line in sums_path.read_text().splitlines():
        digest, name = line.split(maxsplit=1)
        sums[name.lstrip("*")] = digest
    values = {
        "@VERSION@": version,
        "@DATE@": datetime.date.today().isoformat(),
        "@SHA256_MACOS@": sums["optimize-image-folder-macos-universal.tar.gz"],
        "@SHA256_SETUP@": sums["OptimizeImageFolder-Setup.exe"],
    }

    def render(template, dest):
        text = template.read_text(encoding="utf-8")
        for key, value in values.items():
            text = text.replace(key, value)
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(text.encode("utf-8"))
        print(dest)

    render(ROOT / "packaging/homebrew/optimize-image-folder.rb.in", out / "homebrew/optimize-image-folder.rb")
    winget = out / "winget/manifests/t/TsubasaTommy/OptimizeImageFolder" / version
    for template in sorted((ROOT / "packaging/winget").glob("*.yaml.in")):
        render(template, winget / template.name.removesuffix(".in"))


if __name__ == "__main__":
    main()

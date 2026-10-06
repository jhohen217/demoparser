"""Build the explicit release targets without cleaning or copying caches."""
import argparse
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--parser-target-dir", type=Path, help="Optional existing parser cache, separate from the writer cache")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--browser", action="store_true")
    args = parser.parse_args()
    cache = args.target_dir.resolve() if args.target_dir else ROOT / "target"
    output = ROOT / "out"
    output.mkdir(exist_ok=True)
    manifests = [("DemoWriter", ["demo-writer", "schedule-entity-fields", "checkpoint-insert-entities", "audit-entity-fields", "inspect-entity-events", "light-demo-probe"]), ("src/demoparser_rust", ["Demoparser"])]
    if not (ROOT / "DemoWriter/src/bin/light-demo-probe.rs").exists():
        manifests[0][1].remove("light-demo-probe")
    if args.browser:
        manifests[1][1].append("CollectionBrowser")
    for crate, binaries in manifests:
        crate_cache = args.parser_target_dir.resolve() if crate == "src/demoparser_rust" and args.parser_target_dir else cache
        command = ["cargo", "build", "--release", "--locked", "--target-dir", str(crate_cache), "-j", "2"]
        if args.offline:
            command.append("--offline")
        for name in binaries:
            command += ["--bin", name]
        subprocess.run(command, cwd=ROOT / crate, check=True)
        for name in binaries:
            filename = name + (".exe" if sys.platform == "win32" else "")
            shutil.copyfile(crate_cache / "release" / filename, output / filename)
    shutil.copyfile(ROOT / "src/demoparser_rust/config.ini.example", output / "config.ini")
    shutil.copyfile(ROOT / "README.md", output / "README.md")
    shutil.copyfile(ROOT / "LICENSE", output / "LICENSE")
    shutil.copyfile(ROOT / "RUNNING.md", output / "RUNNING.md")
    docs = output / "docs"
    docs.mkdir(exist_ok=True)
    for file in (ROOT / "src/demoparser_rust/docs").glob("*.md"):
        shutil.copyfile(file, docs / file.name)
    for file in (ROOT / "docs").glob("*.md"):
        shutil.copyfile(file, docs / file.name)
    print(f"Release files: {output}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Build the native single-EXE development viewer, NOT a release-ready artifact."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
TARGET = "x86_64-pc-windows-gnullvm"
SYSTEM_DLLS = {
    "kernel32.dll", "ntdll.dll", "user32.dll", "gdi32.dll", "advapi32.dll",
    "bcrypt.dll", "bcryptprimitives.dll", "ws2_32.dll", "ucrtbase.dll", "msvcrt.dll", "dbghelp.dll",
    "ole32.dll", "oleaut32.dll", "combase.dll", "shell32.dll", "comdlg32.dll", "comctl32.dll", "dwmapi.dll",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", required=True, type=Path, help="verified llvm-mingw directory")
    args = parser.parse_args()
    toolchain = args.toolchain.resolve(strict=True)
    linker = toolchain / "bin/x86_64-w64-mingw32-clang"
    readobj = toolchain / "bin/llvm-readobj"
    if not linker.is_file() or not readobj.is_file():
        raise SystemExit("llvm-mingw clang/readobj not found")
    environment = os.environ.copy()
    environment["CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER"] = str(linker)
    # No vendored runtime DLLs. System UCRT/API-set imports are still allowed.
    flags = "-C target-feature=+crt-static -C link-arg=-Wno-unused-command-line-argument"
    environment["RUSTFLAGS"] = (environment.get("RUSTFLAGS", "") + " " + flags).strip()
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT, env=environment,
    ))
    subprocess.run(["cargo", "build", "-p", "ncat-windows", "--target", TARGET,
                    "--release", "--locked"], cwd=ROOT, env=environment, check=True)
    source = Path(metadata["target_directory"]) / TARGET / "release/ncat-windows.exe"
    imports = subprocess.check_output([str(readobj), "--coff-imports", str(source)], text=True)
    names = sorted(set(re.findall(r"^\s*Name:\s*(\S+\.dll)\s*$", imports, re.MULTILINE | re.IGNORECASE)))
    if not names:
        raise SystemExit("PE import inspection returned no DLL names")
    for name in names:
        lower = name.lower()
        if lower not in SYSTEM_DLLS and not lower.startswith(("api-ms-win-", "ext-ms-win-")):
            raise SystemExit(f"Non-system DLL import is forbidden: {name}")
    directory = ROOT / "target/windows-single"
    directory.mkdir(exist_ok=True)
    if any(path.name != "nCatImage.exe" for path in directory.iterdir()):
        raise SystemExit("Deployment directory must contain only the native executable")
    destination = directory / "nCatImage.exe"
    shutil.copy2(source, destination)
    # The PE is the entire deployment folder payload. Metadata remains outside it.
    with destination.open("rb") as file:
        digest = hashlib.file_digest(file, "sha256").hexdigest()
    report = {"stage": "native-viewer-development-not-release-ready", "target": TARGET, "sha256": digest,
              "bytes": destination.stat().st_size, "system_imports": names}
    (ROOT / "target/windows-single-build.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    print(destination)


if __name__ == "__main__":
    main()

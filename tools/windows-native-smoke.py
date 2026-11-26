#!/usr/bin/env python3
"""Native Windows same-EXE containment/pipeline/GUI checks, using generated fixtures only."""
import argparse
import binascii
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import struct
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "win32" or os.environ.get("WINEPREFIX"):
        raise SystemExit("Run on native Windows, not Wine")
    work = Path(tempfile.mkdtemp(prefix="ncat-native-"))
    deployed = work / "deployment 한글 space"
    deployed.mkdir()
    image = deployed / "nCatImage.exe"
    shutil.copy2(args.image.resolve(), image)
    fixture = work / "fixtures 한글"
    subprocess.run([sys.executable, str(Path(__file__).with_name("smoke-fixtures.py")), str(fixture)], check=True)
    png = (fixture / "page1.png").read_bytes()
    text = b"tEXtTitle\0<literal> metadata smoke"
    chunk = struct.pack(">I", len(text) - 4) + text + struct.pack(">I", binascii.crc32(text))
    (fixture / "metadata.png").write_bytes(png[:33] + chunk + png[33:])
    # Own two-color/two-frame looping 1x1 GIF; no fixture downloads.
    gif = bytearray(b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\x00\x00\x00\xff\x00")
    gif.extend(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00")
    for pixel in [b"\x44\x01", b"\x4c\x01"]:
        gif.extend(b"\x21\xf9\x04\x00\x05\x00\x00\x00")
        gif.extend(b"\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02" + pixel + b"\x00")
    gif.extend(b"\x3b")
    (fixture / "animation.gif").write_bytes(gif)
    sources = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in fixture.iterdir()}
    before = hashlib.sha256(image.read_bytes()).hexdigest()
    results = []
    cases = [
        (["--self-test"], 0), (["--worker"], 2),
        (["--smoke-window"], 0),
        (["--smoke-resize"], 0),
        (["--smoke-resize-frame", str(fixture / "page1.png")], 0),
        (["--smoke-metadata-window"], 0),
        (["--smoke-metadata-window", str(fixture / "metadata.png")], 0),
        (["--smoke-pipeline", str(fixture / "page1.png")], 0),
        (["--smoke-pipeline", str(fixture / "sample.cbz")], 0),
        (["--smoke-frame", str(fixture / "sample.cbz")], 0),
        (["--smoke-animation", str(fixture / "animation.gif")], 0),
        (["--smoke-navigation", str(fixture / "sample.cbz")], 0),
        (["--smoke-modal-load", str(fixture / "sample.cbz")], 0),
    ]
    print(f"Native Windows results: {work}", flush=True)
    for index, (arguments, expected) in enumerate(cases):
        log_path = work / f"{index}.log"
        with log_path.open("wb") as log:
            result = subprocess.run([str(image), *arguments], cwd=work, stdout=log,
                                    stderr=subprocess.STDOUT, timeout=120)
        assert result.returncode == expected, f"{arguments[0]}: exit {result.returncode}; inspect {log_path}"
        if arguments[0] == "--smoke-window":
            assert b"window_dpi_smoke=ok per_monitor_v2=true" in log_path.read_bytes(), log_path
        assert list(deployed.iterdir()) == [image], "runtime payload extracted"
        assert hashlib.sha256(image.read_bytes()).hexdigest() == before
        assert all(hashlib.sha256(p.read_bytes()).hexdigest() == digest for p, digest in sources.items())
        results.append({"test": arguments[0], "exit": result.returncode})
    (work / "result.json").write_text(json.dumps({
        "stage": "native-integration-not-security-certification", "tests": results,
        "not_tested": ["Job memory enforcement under exhaustion", "parent crash cleanup",
                       "NTFS ACL attacks", "DPI/accessibility", "real keyboard/mouse input"],
    }, indent=2) + "\n")
    print("Native integration passed; outstanding security/input gates still apply.")


if __name__ == "__main__":
    main()

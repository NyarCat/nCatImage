"""Shared synthetic fixtures for opt-in desktop/network checks; no user data."""
import argparse
import binascii
from pathlib import Path
import struct
import tempfile
import zipfile
import zlib


def arguments(description):
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--image", required=True, type=Path, help="GUI executable or AppImage; sibling worker required")
    args = parser.parse_args()
    args.image = args.image.resolve(strict=True)
    return args


def work_directory(prefix):
    preferred = Path("/tmp/opencode")
    return Path(tempfile.mkdtemp(prefix=prefix, dir=preferred if preferred.is_dir() else None))


def image_command(image, *args):
    return [str(image), *(["--appimage-extract-and-run"] if image.suffix == ".AppImage" else []), *map(str, args)]


def solid_png(width, height, rgb, title=None):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", binascii.crc32(kind + data) & 0xFFFFFFFF)

    raw = (b"\0" + bytes(rgb) * width) * height
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + (chunk(b"tEXt", b"Title\0" + title.encode("latin-1")) if title else b"")
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def fixtures(work):
    root = work / "DO-NOT-LOG-fixtures"
    root.mkdir()
    folder = root / "DO-NOT-LOG folder"
    folder.mkdir()

    pages = []
    for index, number in enumerate([1, 2, 10]):
        data = solid_png(64, 96, [60 + index * 60, 130, 180])
        page = root / f"DO-NOT-LOG-page{number}.png"
        page.write_bytes(data)
        (folder / page.name).write_bytes(data)
        pages.append(page)
    book = root / "DO-NOT-LOG book.cbz"
    with zipfile.ZipFile(book, "x", compression=zipfile.ZIP_DEFLATED) as archive:
        for page in pages:
            entry = zipfile.ZipInfo(page.name, date_time=(2000, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(entry, page.read_bytes())
    bad = root / "DO-NOT-LOG invalid.cbz"
    bad.write_bytes(b"not an archive")
    return {"root": root, "folder": folder, "book": book, "last": pages[-1], "bad": bad}


def check_privacy(text):
    for secret in ["DO-NOT-LOG", "file://", "smb://", "sftp://", "ftp://", "127.0.0.1", "anonymous"]:
        assert secret not in text, "diagnostic log contains source/endpoint data"

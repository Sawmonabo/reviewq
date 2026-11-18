"""Portable byte manifests and exclusive fixture export."""

import hashlib
import json
import shutil
from pathlib import Path


def encode_json(value):
    return (json.dumps(value, sort_keys=True, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def encode_lines(records):
    return b"".join(
        (json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
        for record in records
    )


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def index_files(files):
    return {name: {"bytes": len(data), "sha256": sha256(data)} for name, data in files.items()}


def record_extents(data):
    lines = data.split(b"\n")
    count = len(lines) - data.endswith(b"\n") if data else 0
    widths = [len(line) - 1 if index < len(lines) - 1 and line.endswith(b"\r")
              else len(line) for index, line in enumerate(lines)]
    return {"bytes": len(data), "sha256": sha256(data), "lines": count,
            "max_line_bytes": max(widths),
            "tail_hex": data[-64:].hex()}


def write_new_directory(output, files):
    output.mkdir(mode=0o700)
    try:
        for name, data in files.items():
            path = output / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as stream:
                stream.write(data)
    except BaseException:
        shutil.rmtree(output)
        raise


def read_manifest(directory):
    manifest = json.loads((directory / "manifest.json").read_bytes())
    if manifest["version"] != 1:
        raise ValueError(f"unsupported fixture manifest version in {directory}")
    for name, expected in manifest["files"].items():
        path = Path(name)
        if path.is_absolute() or ".." in path.parts or path.as_posix() != name:
            raise ValueError(f"invalid fixture path {name!r}")
        data = (directory / path).read_bytes()
        if expected != {"bytes": len(data), "sha256": sha256(data)}:
            raise ValueError(f"fixture byte manifest mismatch for {name}")
    return manifest

#!/usr/bin/env python3
"""Fill nfpm's missing Arch metadata and update the corresponding mtree hashes."""

import gzip
import hashlib
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def finalize(package):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        subprocess.run(["bsdtar", "-xf", package, "-C", directory], check=True)
        pkginfo = root / ".PKGINFO"
        lines = pkginfo.read_text().splitlines()
        lines = [line for line in lines if not line.startswith("packager = ")]
        lines.append(f"packager = {os.environ['PACKAGE_MAINTAINER']}")
        lines.extend(
            f"optdepend = {dependency}"
            for dependency in (
                "pipewire: system audio capture",
                "wireplumber: PipeWire session and audio routing management",
                "gnome-shell-extension-appindicator: system tray support on GNOME",
            )
        )
        data = ("\n".join(lines) + "\n").encode()
        pkginfo.write_bytes(data)

        mtree = root / ".MTREE"
        lines = gzip.decompress(mtree.read_bytes()).decode().splitlines()
        attributes = {
            "size": str(len(data)),
            "md5digest": hashlib.md5(data, usedforsecurity=False).hexdigest(),
            "sha256digest": hashlib.sha256(data).hexdigest(),
        }
        for index, line in enumerate(lines):
            if line.startswith("./.PKGINFO "):
                fields = dict(field.split("=", 1) for field in line.split()[1:])
                fields.update(attributes)
                lines[index] = "./.PKGINFO " + " ".join(
                    f"{key}={value}" for key, value in fields.items()
                )
        mtree.write_bytes(gzip.compress(("\n".join(lines) + "\n").encode(), mtime=0))
        timestamp = int(os.environ["SOURCE_DATE_EPOCH"])
        for path in (pkginfo, mtree):
            os.utime(path, (timestamp, timestamp))
        subprocess.run(
            [
                "bsdtar", "--zstd", "--uid", "0", "--gid", "0",
                "--uname", "root", "--gname", "root", "-cf", package,
                "-C", directory, ".PKGINFO", ".MTREE", "usr",
            ],
            check=True,
        )


if __name__ == "__main__":
    finalize(str(Path(sys.argv[1]).resolve()))

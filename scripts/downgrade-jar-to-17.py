#!/usr/bin/env python3
"""Downgrade a Java 21 (class file 65.0) jar so it runs on JDK 17 (61.0).

Usage: python3 scripts/downgrade-jar-to-17.py <input.jar> <output.jar>

Only rewrites the class file version of classes compiled at major 65 to 61.
Uses the system unzip/zip tools so overlapping entries (duplicate LICENSE
files inside shadow jars) are tolerated. It does not rewrite bytecode, so a
class that really uses Java 18+ bytecode features would still fail at runtime.
"""
import os
import struct
import subprocess
import sys
import tempfile


def run(cmd: list, cwd: str = None) -> None:
    subprocess.run(cmd, check=True, capture_output=True, cwd=cwd)


def downgrade(src_jar: str, out_jar: str) -> int:
    work = tempfile.mkdtemp(prefix="downgrade-jar-")
    extract_dir = os.path.join(work, "extract")
    os.makedirs(extract_dir)
    run(["unzip", "-oq", src_jar, "-d", extract_dir])

    changed = 0
    for dirpath, _, files in os.walk(extract_dir):
        for fn in files:
            if not fn.endswith(".class"):
                continue
            path = os.path.join(dirpath, fn)
            with open(path, "r+b") as f:
                data = f.read(8)
                if len(data) < 8 or data[:4] != b"\xca\xfe\xba\xbe":
                    continue
                major = struct.unpack(">H", data[6:8])[0]
                if major > 61:
                    f.seek(6)
                    f.write(struct.pack(">H", 61))
                    changed += 1

    run(["zip", "-q", "-r", "-9", out_jar, "."], cwd=extract_dir)
    subprocess.run(["rm", "-rf", work])
    return changed


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(2)
    n = downgrade(sys.argv[1], sys.argv[2])
    print(f"downgraded {n} classes; wrote {sys.argv[2]}")

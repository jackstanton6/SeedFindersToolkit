"""Generates ground-truth fixtures with the real game code (oracle/Oracle.java).

Usage: python tests/run_oracle.py [version] [mode ...]
Needs `python -m sft data <version>` to have run first (it leaves the extracted server jar and
its libraries under cache/datagen-<version>/). Output: cache/oracle/<mode>-<seed>.json
"""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SEEDS = [0, 12345, -4172144997902289642, 8675309]
COUNTS = {"climate": 3000, "chunkbiomes": 20, "heights": 400, "structures": 48, "slime": 20000, "blockbiomes": 20000}


def classpath(version: str) -> str:
    base = ROOT / "cache" / f"datagen-{version}"
    jars = [base / "versions" / version / f"server-{version}.jar", *sorted((base / "libraries").rglob("*.jar"))]
    return os.pathsep.join(str(j) for j in jars)


def run(version: str, mode: str, seed: int) -> Path:
    out = ROOT / "cache" / "oracle" / f"{mode}-{seed}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["java", "-cp", classpath(version), str(ROOT / "oracle" / "Oracle.java"), mode, str(seed), str(COUNTS[mode]), str(out)],
        check=True, cwd=ROOT / "cache",
    )
    return out


if __name__ == "__main__":
    version = sys.argv[1] if len(sys.argv) > 1 else "26.3"
    modes = sys.argv[2:] or ["climate", "chunkbiomes", "blockbiomes", "heights"]
    for mode in modes:
        for seed in SEEDS:
            print("wrote", run(version, mode, seed))

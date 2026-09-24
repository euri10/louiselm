#!/usr/bin/env python3
"""Identify a failed capture without changing the production reader deadline."""
import importlib.util
import os
from pathlib import Path
import shutil
import tempfile


source = Path(__file__).resolve().parent.parent / "usage-20260924/benchmark.py"
spec = importlib.util.spec_from_file_location("benchmark", source)
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)
sqlite = shutil.which("sqlite3")
with tempfile.TemporaryDirectory(prefix="sawgo-capture-") as scratch:
    root = Path(scratch)
    directory = root / "synthetic-251800"
    directory.mkdir(mode=0o700)
    benchmark.nvim(directory, "seed")
    path = directory / "turns.sqlite3"
    benchmark.fixture(path, 251800)
    wrapper = root / "bin"
    wrapper.mkdir()
    (wrapper / "sqlite3").symlink_to(source)
    env = {**os.environ, "PATH": str(wrapper) + os.pathsep + os.environ["PATH"],
           "LOUISELM_BENCH_SQLITE": sqlite}
    for case in benchmark.cases(path):
        print(case["name"], flush=True)
        benchmark.dump(directory / "cases.json", [case])
        try:
            _, ms = benchmark.elapsed(lambda: benchmark.nvim(directory, "capture", env))
            print(f"capture completed in {ms:.2f} ms (includes Neovim startup)", flush=True)
        except RuntimeError as error:
            print(str(error), flush=True)
            raw = (directory / (case["name"] + ".sql")).read_bytes()
            out, ms = benchmark.elapsed(lambda: benchmark.run(
                [sqlite, "-readonly", "-batch", "-bail", "-json", "-nofollow", "-init", "/dev/null", str(path)], input=raw))
            print(f"Direct SQL: {ms:.2f} ms, {len(out)} bytes; reader deadline remains 5000 ms", flush=True)

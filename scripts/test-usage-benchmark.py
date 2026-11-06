#!/usr/bin/env python3
"""Offline failure-artifact checks using real benchmark capture and measurement."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / "docs/performance/usage-20260924/benchmark.py"
spec = importlib.util.spec_from_file_location("benchmark", HARNESS)
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class CaptureFailureTest(unittest.TestCase):
    def test_capture_retains_safe_evidence(self):
        for action, state, code, child_signal, wrapper_signal in [
            ("raise SystemExit(23)", "exited", 23, None, None),
            ("os.kill(os.getpid(), signal.SIGTERM)", "exited", -15, 15, None),
            ("os.kill(os.getppid(), signal.SIGTERM); time.sleep(30)", "interrupted", None, None, 15),
            ("time.sleep(30)", "interrupted", None, None, 15),
        ]:
            with self.subTest(state=state, code=code), tempfile.TemporaryDirectory() as scratch:
                directory = Path(scratch)
                bench.nvim(directory, "seed")
                bench.fixture(directory / "turns.sqlite3", 63)
                bench.dump(directory / "cases.json", bench.cases(directory / "turns.sqlite3"))
                binary = directory / "bin"
                binary.mkdir()
                (binary / "sqlite3").symlink_to(HARNESS)
                fake = directory / "fail-sqlite"
                fake.write_text(
                    "#!/usr/bin/env python3\nimport os, signal, sys, time\n"
                    "if os.environ['LOUISELM_BENCH_CASE'] == 'summary':\n"
                    " print('SECRET PAYLOAD', file=sys.stderr, flush=True)\n"
                    f" {action}\n"
                    f"os.execv({shutil.which('sqlite3')!r}, ['sqlite3', *sys.argv[1:]])\n"
                )
                fake.chmod(0o700)
                env = {**os.environ, "PATH": str(binary) + os.pathsep + os.environ["PATH"],
                       "LOUISELM_BENCH_SQLITE": str(fake)}
                with self.assertRaises(RuntimeError) as caught:
                    bench.nvim(directory, "capture", env)
                evidence = caught.exception.evidence
                self.assertEqual(evidence["case"], "summary")
                self.assertEqual(evidence["phase"], "capture")
                self.assertEqual(evidence["sqlite"]["state"], state)
                self.assertEqual(evidence["sqlite"]["returncode"], code)
                self.assertEqual(evidence["sqlite"]["signal"], child_signal)
                self.assertEqual(evidence["sqlite"]["wrapper_signal"], wrapper_signal)
                self.assertGreater(evidence["completed"]["picker"]["capture_ms"], 0)
                self.assertNotEqual(evidence["nvim"]["returncode"], 0)
                encoded = json.dumps(evidence)
                for forbidden in ("SECRET", "SELECT", "model-", "session-", str(directory)):
                    self.assertNotIn(forbidden, encoded)
                self.assertEqual(evidence["sqlite"]["timeout"],
                                 None if state == "interrupted" else state == "timeout")
                if action == "time.sleep(30)":
                    # Outside Neovim, the wrapper's own ten-second deadline fires.
                    result = subprocess.run(
                        [str(binary / "sqlite3"), str(directory / "turns.sqlite3")], input=b"",
                        env={**env, "LOUISELM_BENCH_CASE": "summary"}, capture_output=True, timeout=20,
                    )
                    self.assertEqual(result.returncode, 124)
                    status = json.loads((directory / "summary.status.json").read_text())
                    self.assertEqual(status["state"], "timeout")
                    self.assertTrue(status["timeout"])
                if code == 23:
                    # Exercise the CLI artifact, including a fully completed dataset.
                    nvim_bin = directory / "nvim-bin"
                    nvim_bin.mkdir()
                    launcher = nvim_bin / "nvim"
                    launcher.write_text(
                        "#!/usr/bin/env python3\nimport os, sys\n"
                        "if sys.argv[-1] == 'capture' and sys.argv[-2].endswith('synthetic-126'):\n"
                        f" os.environ['LOUISELM_BENCH_SQLITE'] = {str(fake)!r}\n"
                        f"os.execv({shutil.which('nvim')!r}, ['nvim', *sys.argv[1:]])\n"
                    )
                    launcher.chmod(0o700)
                    result = subprocess.run(
                        [sys.executable, str(HARNESS), "--sizes", "63", "126"],
                        env={**os.environ, "PATH": str(nvim_bin) + os.pathsep + os.environ["PATH"]},
                        capture_output=True, timeout=60,
                    )
                    self.assertEqual(result.returncode, 1, result.stderr.decode())
                    artifact = json.loads(result.stdout)
                    self.assertEqual(artifact["failure"]["dataset"], "synthetic-126")
                    self.assertEqual(artifact["failure"]["case"], "summary")
                    queries = artifact["datasets"]["synthetic-63"]["queries"]
                    self.assertEqual(len(queries), 8)
                    self.assertTrue(all(len(query["api_ms"]["runs"]) == 5 for query in queries.values()))
                    self.assertNotIn("SECRET", result.stdout.decode() + result.stderr.decode())


class MeasureFailureTest(unittest.TestCase):
    def test_measure_retains_safe_progress_and_completed_datasets(self):
        for action, category, nvim_signal in [
            ("raise SystemExit(23)", "query_error", None),
            ("os.kill(os.getppid(), signal.SIGKILL); raise SystemExit(23)", "process_signal", 9),
        ]:
            with self.subTest(category=category), tempfile.TemporaryDirectory() as scratch:
                directory = Path(scratch)
                sqlite_bin = directory / "sqlite-bin"
                sqlite_bin.mkdir()
                fake = sqlite_bin / "sqlite3"
                counter = directory / "calls"
                fake.write_text(
                    "#!/usr/bin/env python3\nimport os, signal, sys\nfrom pathlib import Path\n"
                    "if os.environ.get('LOUISELM_BENCH_CASE') == 'summary':\n"
                    f" counter = Path({str(counter)!r})\n"
                    " count = int(counter.read_text()) + 1 if counter.exists() else 1\n"
                    " counter.write_text(str(count))\n"
                    " if count == 4:\n"
                    "  print('SECRET PAYLOAD', file=sys.stderr, flush=True)\n"
                    f"  {action}\n"
                    f"os.execv({shutil.which('sqlite3')!r}, ['sqlite3', *sys.argv[1:]])\n"
                )
                fake.chmod(0o700)
                nvim_bin = directory / "nvim-bin"
                nvim_bin.mkdir()
                launcher = nvim_bin / "nvim"
                launcher.write_text(
                    "#!/usr/bin/env python3\nimport os, sys\n"
                    "if sys.argv[-1] == 'measure' and sys.argv[-2].endswith('synthetic-126'):\n"
                    f" os.environ['PATH'] = {str(sqlite_bin)!r} + os.pathsep + os.environ['PATH']\n"
                    f"os.execv({shutil.which('nvim')!r}, ['nvim', *sys.argv[1:]])\n"
                )
                launcher.chmod(0o700)
                result = subprocess.run(
                    [sys.executable, str(HARNESS), "--sizes", "63", "126"],
                    env={**os.environ, "PATH": str(nvim_bin) + os.pathsep + os.environ["PATH"]},
                    capture_output=True, timeout=60,
                )
                self.assertEqual(result.returncode, 1, result.stderr.decode())
                artifact = json.loads(result.stdout)
                evidence = artifact["failure"]
                self.assertEqual(evidence["dataset"], "synthetic-126")
                self.assertEqual(evidence["phase"], "measure")
                self.assertEqual(evidence["case"], "summary")
                self.assertEqual(evidence["step"], "api")
                self.assertEqual(evidence["iteration"], 3)
                self.assertEqual(evidence["category"], category)
                self.assertEqual(evidence["nvim"]["signal"], nvim_signal)
                self.assertNotIn("sqlite", evidence, "capture status is not measurement evidence")
                self.assertEqual(len(evidence["completed"]["picker"]["api_ms"]), 5)
                self.assertEqual(len(evidence["completed"]["picker"]["decode_ms"]), 5)
                self.assertEqual(evidence["completed"]["picker"]["render_ms"], [])
                self.assertEqual(len(evidence["partial"]["api_ms"]), 2)
                self.assertEqual(len(evidence["partial"]["decode_ms"]), 2)
                self.assertEqual(evidence["partial"]["render_ms"], [])
                if category == "query_error":
                    self.assertEqual(evidence["error_code"], "storage")
                else:
                    self.assertNotIn("error_code", evidence)
                queries = artifact["datasets"]["synthetic-63"]["queries"]
                self.assertEqual(len(queries), 8)
                self.assertTrue(all(len(query["api_ms"]["runs"]) == 5 for query in queries.values()))
                for forbidden in ("SECRET", "SELECT", "model-", "session-", str(directory)):
                    self.assertNotIn(forbidden, json.dumps(evidence))
                self.assertNotIn("SECRET", result.stdout.decode() + result.stderr.decode())

    def test_measure_nonquery_failures(self):
        for action, category, step, progress in [
            ("(directory / 'summary.out').write_text('SECRET PAYLOAD')", "lua_error", "decode", None),
            ("(directory / 'summary.page.json').write_text('[]')", "result_mismatch", "compare", None),
            ("raise SystemExit(23)", "process_exit", None, "missing"),
            ("(directory / 'measure-progress.json').write_text('SECRET PAYLOAD'); raise SystemExit(23)",
             "process_exit", None, "unreadable"),
        ]:
            with self.subTest(step=step, progress=progress), tempfile.TemporaryDirectory() as scratch:
                directory = Path(scratch)
                bench.nvim(directory, "seed")
                bench.fixture(directory / "turns.sqlite3", 63)
                bench.dump(directory / "cases.json", bench.cases(directory / "turns.sqlite3"))
                binary = directory / "bin"
                binary.mkdir()
                (binary / "sqlite3").symlink_to(HARNESS)
                env = {**os.environ, "PATH": str(binary) + os.pathsep + os.environ["PATH"],
                       "LOUISELM_BENCH_SQLITE": shutil.which("sqlite3")}
                bench.nvim(directory, "capture", env)
                (binary / "sqlite3").unlink()  # Measure uses real SQLite, not the capture wrapper.
                # A previous attempt must not supply the current failure's evidence.
                bench.dump(directory / "measure-progress.json", {"case": "SECRET STALE"})
                launcher = binary / "nvim"
                launcher.write_text(
                    "#!/usr/bin/env python3\nimport os, sys\nfrom pathlib import Path\n"
                    "directory = Path(sys.argv[-2])\n"
                    f"{action}\n"
                    f"os.execv({shutil.which('nvim')!r}, ['nvim', *sys.argv[1:]])\n"
                )
                launcher.chmod(0o700)
                with self.assertRaises(bench.BenchmarkFailure) as caught:
                    bench.nvim(directory, "measure", env)
                evidence = caught.exception.evidence
                self.assertEqual(evidence["phase"], "measure")
                self.assertEqual(evidence["category"], category)
                self.assertNotEqual(evidence["nvim"]["returncode"], 0)
                self.assertNotIn("error_code", evidence)
                self.assertNotIn("sqlite", evidence)
                if step:
                    self.assertEqual(evidence["case"], "summary")
                    self.assertEqual(evidence["step"], step)
                    self.assertEqual(len(evidence["completed"]["picker"]["api_ms"]), 5)
                    self.assertEqual(len(evidence["partial"]["api_ms"]), int(step == "decode"))
                    self.assertEqual(evidence["partial"]["decode_ms"], [])
                    self.assertEqual(evidence["partial"]["render_ms"], [])
                    if step == "decode":
                        self.assertEqual(evidence["iteration"], 1)
                    else:
                        self.assertNotIn("iteration", evidence)
                else:
                    self.assertEqual(evidence["progress"], progress)
                    self.assertEqual(evidence["nvim"]["returncode"], 23)
                    self.assertNotIn("case", evidence)
                for forbidden in ("SECRET", "SELECT", "model-", "session-", str(directory)):
                    self.assertNotIn(forbidden, json.dumps(evidence))


if __name__ == "__main__":
    unittest.main()

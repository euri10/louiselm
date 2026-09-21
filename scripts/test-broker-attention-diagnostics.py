#!/usr/bin/env python3
"""Unprivileged checks for the installed Attention gate's bounded diagnostics."""

from pathlib import Path
import selectors
import subprocess
import sys
import textwrap
import unittest


FIXTURE = Path(__file__).with_name("test-broker-attention.py")


class Diagnostics(unittest.TestCase):
    def probe(self, body):
        setup = ("import runpy, time\n"
                 f"diagnostics = runpy.run_path({str(FIXTURE)!r})['diagnostics']\n")
        result = subprocess.run([sys.executable, "-c", setup + textwrap.dedent(body)],
                                text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_phase_flushes_before_waiting_for_input(self):
        script = ("import runpy, sys\n"
                  "sys.stderr.reconfigure(line_buffering=False, write_through=False)\n"
                  f"diagnostics = runpy.run_path({str(FIXTURE)!r})['diagnostics']\n"
                  "with diagnostics() as phase:\n"
                  "    phase('waiting-for-input')\n"
                  "    input()\n")
        child = subprocess.Popen([sys.executable, "-c", script], text=True,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            with selectors.DefaultSelector() as ready:
                ready.register(child.stderr, selectors.EVENT_READ)
                self.assertTrue(ready.select(timeout=3), "phase buffered until process exit")
            self.assertRegex(child.stderr.readline(), r"\[broker-attention\] \d+\.\d{3}s waiting-for-input")
            stdout, stderr = child.communicate("\n", timeout=3)
            self.assertEqual(child.returncode, 0, stderr)
            self.assertEqual((stdout, stderr), ("", ""))
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=3)

    def test_stall_dumps_once_without_payload_or_process_exit(self):
        result = self.probe('''
            with diagnostics(timeout=0.05) as phase:
                secret = "synthetic-private-capability"
                phase("synthetic-wait")
                def stalled():
                    time.sleep(0.2)
                stalled()
            print("still-running")
        ''')
        self.assertRegex(result.stderr, r"\[broker-attention\] \d+\.\d{3}s synthetic-wait")
        self.assertEqual(result.stderr.count("Timeout ("), 1)
        self.assertIn("in stalled", result.stderr)
        self.assertNotIn("synthetic-private-capability", result.stderr + result.stdout)
        self.assertEqual(result.stdout, "still-running\n")

    def test_normal_and_exception_exit_cancel_the_dump(self):
        result = self.probe('''
            for fail in (False, True):
                try:
                    with diagnostics(timeout=0.05) as phase:
                        phase("synthetic-exit")
                        if fail:
                            raise RuntimeError("synthetic-private-error")
                except RuntimeError:
                    pass
                time.sleep(0.1)
        ''')
        self.assertEqual(result.stderr.count("synthetic-exit"), 2)
        self.assertNotIn("Timeout (", result.stderr)
        self.assertNotIn("synthetic-private-error", result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()

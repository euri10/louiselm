#!/usr/bin/env python3
"""Small clock-verifier double for ordinary editor lifecycle tests."""

import os
import sys
import time

mode = sys.argv[1]
if sys.argv[-2:] == ["phone-control", "verify"]:
    print('{"v":1,"type":"verified"}')
    sys.exit(0)
if "verify-process" in sys.argv:
    index = sys.argv.index("verify-process")
    assert sys.argv[index - 1] == "phone-control"
    assert sys.argv[index + 1] == "--pid" and sys.argv[index + 3] == "--editor-pid"
    assert sys.argv[index + 5] == "--namespace"
    pid, editor_pid, namespace = sys.argv[index + 2], sys.argv[index + 4], sys.argv[index + 6]
    actual, expected = os.fstat(0), os.stat(f"/proc/{pid}/stat")
    assert (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
    actual, expected = os.fstat(2), os.stat(f"/proc/{editor_pid}/status")
    assert (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
    assert namespace == os.readlink(f"/proc/{pid}/ns/pid") == os.readlink("/proc/self/ns/pid")
    if mode == "process_hold":
        with open(sys.argv[2], "w", encoding="utf-8") as marker:
            marker.write(str(os.getpid()))
        time.sleep(30)
    elif mode == "process_malformed":
        print('{"v":1,"type":"process_verified"')
    else:
        print('{"v":1,"type":"process_verified"}')
    sys.exit(0)
assert sys.argv[-2:] == ["phone-control", "verify-clock"]
actual = os.fstat(0)
expected = os.stat("/proc/uptime")
assert (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
if mode == "hold":
    with open(sys.argv[2], "w", encoding="utf-8") as marker:
        marker.write(str(os.getpid()))
    time.sleep(30)
elif mode == "malformed":
    print('{"v":1,"type":"clock_verified"')
else:
    print('{"v":1,"type":"clock_verified"}')

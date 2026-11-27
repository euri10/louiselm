#!/usr/bin/env python3
"""Small clock-verifier double for ordinary editor lifecycle tests."""

import os
import sys
import time

mode = sys.argv[1]
if sys.argv[-2:] == ["phone-control", "verify"]:
    print('{"v":1,"type":"verified"}')
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

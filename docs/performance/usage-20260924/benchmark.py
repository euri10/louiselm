#!/usr/bin/env python3
"""Measure shipped usage readers. Stdlib only; private scratch data is removed on exit."""
import argparse
from datetime import datetime, timedelta
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
REPEATS = 5


def dump(path, value):
    path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")))


def run(args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=kwargs.pop("timeout", 120), **kwargs).stdout


def nvim(directory, mode, env=None):
    try:
        run(["nvim", "--headless", "--noplugin", "-u", "NONE", "-l",
             str(HERE / "benchmark.lua"), str(directory), mode], cwd=ROOT, env=env, timeout=300)
    except subprocess.CalledProcessError as error:
        raise RuntimeError(error.stderr.decode()) from error


def elapsed(action):
    start = time.perf_counter_ns()
    result = action()
    return result, (time.perf_counter_ns() - start) / 1e6


def distribution(values):
    return {"runs": values, "median": statistics.median(values),
            "min": min(values), "max": max(values)}


def fixture(path, count):
    """63 typed tuples; dimensions stay varied, Sessions/time grow with history."""
    db = sqlite3.connect(path)
    encode = lambda x: json.dumps(x, sort_keys=True, separators=(",", ":"))
    for i in range(count):
        k = i % 63
        options = {"model": f"model-{k % 23}", "reasoning": ["low", "medium", "high"][k % 3]}
        if k % 4:
            options["enabled"] = [False, True, "false"][k % 3]
        stamp = (datetime(2026, 9, 1) + timedelta(minutes=10 * i)).strftime("%Y-%m-%dT%H:%M:%SZ")
        agent, provider, session = f"agent-{k % 9}", f"provider-{k % 10}", f"session-{i // 8}"
        currency = "EUR" if i % 2 else "USD"
        baseline = {"amount": 1, "currency": currency} if i % 5 == 0 else None
        db.execute("INSERT INTO turns VALUES(?,?,?,?,?,?,?,?)",
                   (str(i), agent, provider, session, stamp, encode(options), encode(options["model"]),
                    encode(baseline) if baseline else None))
        events = [(str(i), 1, "dispatch", stamp, "{}")]
        if baseline:
            events.append((str(i), 2, "cost", stamp, encode({"cost": {"amount": 2, "currency": currency}})))
        if i % 503:
            data = {"outcome": "cancelled" if i % 73 == 0 else "completed", "peer_response": True}
            if i % 40:  # Independent field coverage, including measured zero.
                data["usage"] = {"total_tokens": 100, "input_tokens": 80}
                if i % 7:
                    data["usage"]["output_tokens"] = 20
                if i % 11 == 0:
                    data["usage"]["thought_tokens"] = 0
            events.append((str(i), 3, "outcome", stamp, encode(data)))
        db.executemany("INSERT INTO turn_events VALUES(?,?,?,?,?)", events)
        if i % 997 == 0:
            db.execute("INSERT INTO option_events VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                       (str(i), session, 2 * i + 1, agent, session, stamp, encode(options),
                        encode({**options, "reasoning": "changed"}), "notification", None, str(i)))
        if i % 3 == 0:
            db.execute("INSERT INTO option_events VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                       (f"between-{i}", session, 2 * i + 2, agent, session, stamp, encode(options),
                        encode(options), "notification", None, None))
    db.commit()
    assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    db.close()


def cases(path):
    with sqlite3.connect(path) as db:
        agent, provider, options = db.execute(
            "SELECT agent,provider,options FROM turns GROUP BY agent,provider,options ORDER BY count(*) DESC,agent,provider,options LIMIT 1"
        ).fetchone()
        options = json.loads(options)
        candidates = []
        # Actual picker contract: vary one Model, hold every other option fixed.
        for (model,) in db.execute("SELECT DISTINCT model FROM turns WHERE model IS NOT NULL ORDER BY model LIMIT 12"):
            candidates.append({"agent": agent, "provider": provider, "options": {**options, "model": json.loads(model)}})
        if not any(c["options"] == options for c in candidates):
            candidates.append({"agent": agent, "provider": provider, "options": options})
        turn, session = db.execute("SELECT id,acp_session_id FROM turns WHERE agent=? LIMIT 1", (agent,)).fetchone()
        start = db.execute("SELECT min(prepared_at) FROM turns").fetchone()[0]
    filters = {"agent": agent, "provider": provider, **{f"option:{key}": value for key, value in options.items()}}
    return [
        {"name": "picker", "cohorts": candidates},
        {"name": "summary", "query": {}},
        {"name": "grouped", "query": {"group_by": ["agent", "model", "option:reasoning"], "bucket": "day"}},
        {"name": "filtered", "query": {"filters": filters, "from": start}},
        {"name": "turns", "query": {"view": "turns", "limit": 25}},
        {"name": "dimensions", "query": {"view": "dimensions", "limit": 25}},
        {"name": "session_events", "query": {"view": "events", "filters": {"agent": agent, "session": session}}},
        {"name": "turn_events", "query": {"view": "events", "turn_id": turn}},
    ]


def statements(sql):
    pending = ""
    for line in sql.splitlines(True):
        if line.startswith("."):
            continue
        for character in line:
            pending += character
            if character == ";" and sqlite3.complete_statement(pending):
                yield pending
                pending = ""
    assert not pending.strip(), "unterminated captured SQL"


def execute(db, sql):
    rows = []
    for statement in sql:
        cursor = db.execute(statement)
        if cursor.description:
            rows.extend(dict(row) for row in cursor)
    return rows


def normalized(rows):
    return [{**row, "result": json.loads(row["result"])} if "result" in row else row for row in rows]


def profile_statements(path, sql):
    """Diagnostic pass only: timings are separate from the benchmark samples."""
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    profiles = []
    for statement in sql:
        clean = re.sub(r"--[^\n]*", "", statement).strip()
        label = " ".join(clean.split()[:5]) if clean.startswith("CREATE TEMP TABLE") else clean.split()[0]
        plan = [row[3] for row in db.execute("EXPLAIN QUERY PLAN " + statement)]
        _, ms = elapsed(lambda: db.execute(statement).fetchall())
        profiles.append({"statement": label, "ms": ms, "plan": plan})
    db.close()
    return profiles


def measure(directory, sqlite, capture_env, profile=False):
    path = directory / "turns.sqlite3"
    queries = cases(path)
    dump(directory / "cases.json", queries)
    nvim(directory, "capture", capture_env)
    if directory.name.startswith("synthetic-"):
        count = int(directory.name.removeprefix("synthetic-"))
        page = json.loads((directory / "summary.page.json").read_text())
        summary = page["summary"]
        coverage = sum(i % 503 != 0 and i % 40 != 0 for i in range(count))
        assert summary["turns"] == count
        assert summary["tokens"]["total_tokens"]["samples"] == coverage
        assert summary["tokens"]["total_tokens"]["total"] == coverage * 100
        assert page["mixed_turns"] == len(range(0, count, 997))
        assert summary["outcomes"]["unobserved"] == len(range(0, count, 503))
    nvim(directory, "measure")
    lua = json.loads((directory / "lua-results.json").read_text())
    results = {}
    for case in queries:
        name = case["name"]
        print(f"  {name}", file=sys.stderr, flush=True)
        raw = (directory / f"{name}.sql").read_bytes()
        sql = list(statements(raw.decode()))
        reference = normalized(json.loads((directory / f"{name}.out").read_bytes() or b"[]"))
        cli, fresh, warm, transfer = [], [], [], []
        connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        connection.row_factory = sqlite3.Row
        for _ in range(REPEATS):
            out, ms = elapsed(lambda: run([sqlite, "-readonly", "-batch", "-bail", "-json", "-nofollow", "-init", "/dev/null", str(path)], input=raw))
            assert normalized(json.loads(out or b"[]")) == reference, "CLI changed cohort/coverage"
            cli.append(ms)
            db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
            db.row_factory = sqlite3.Row
            rows, ms = elapsed(lambda: execute(db, sql))
            assert normalized(rows) == reference, "embedded SQLite changed cohort/coverage"
            fresh.append(ms)
            db.close()
            rows, ms = elapsed(lambda: execute(connection, sql))
            assert normalized(rows) == reference, "warm SQLite changed cohort/coverage"
            warm.append(ms)
            for (table,) in connection.execute("SELECT name FROM sqlite_temp_master WHERE type='table'").fetchall():
                connection.execute('DROP TABLE "' + table.replace('"', '""') + '"')
            _, ms = elapsed(lambda: run(["cat", str(directory / f"{name}.out")]))
            transfer.append(ms)
        connection.close()
        results[name] = {"bytes": len(out), "sqlite_cli_ms": distribution(cli),
                         "result_sha256": hashlib.sha256(json.dumps(reference, sort_keys=True).encode()).hexdigest(),
                         "sqlite_fresh_connection_ms": distribution(fresh),
                         "sqlite_reused_connection_ms": distribution(warm[1:]),
                         "cat_process_and_transfer_ms": distribution(transfer),
                         **{k: distribution(v) for k, v in lua[name].items() if v}}
        if profile:
            results[name]["profile"] = profile_statements(path, sql)
    with sqlite3.connect(path) as db:
        shape = dict(zip(["turns", "agents", "providers", "models", "sessions", "tuples"], db.execute(
            "SELECT count(*),count(distinct agent),count(distinct provider),count(distinct model),count(distinct acp_session_id),count(distinct options) FROM turns").fetchone()))
        shape["events"] = dict(db.execute("SELECT kind,count(*) FROM turn_events GROUP BY kind"))
        shape["option_events"] = db.execute("SELECT count(*) FROM option_events").fetchone()[0]
        shape["mixed_turns"] = db.execute("SELECT count(distinct turn_id) FROM option_events").fetchone()[0]
        shape["days"] = db.execute("SELECT count(distinct substr(prepared_at,1,10)) FROM turns").fetchone()[0]
        shape["token_coverage"] = db.execute("SELECT count(*) FROM turn_events WHERE kind='outcome' AND json_type(data,'$.usage.total_tokens')='integer'").fetchone()[0]
    return {"shape": shape, "database_bytes": path.stat().st_size, "queries": results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current", type=Path, help="Optional current recorder DB; only a private snapshot is measured")
    parser.add_argument("--sizes", nargs="+", type=int, default=[2518, 25180, 251800])
    parser.add_argument("--profile", action="store_true", help="Include separate per-statement timings and query plans")
    args = parser.parse_args()
    assert all(0 < size <= 86400 * 28 for size in args.sizes)
    sqlite = shutil.which("sqlite3")
    assert sqlite
    result = {"environment": {"platform": platform.platform(), "python": platform.python_version(),
              "sqlite_embedded": sqlite3.sqlite_version, "sqlite_cli": run([sqlite, "--version"]).decode().strip(),
              "sqlite_executable": sqlite,
              "nvim": run(["nvim", "--version"]).decode().splitlines()[0],
              "revision": run(["git", "rev-parse", "HEAD"], cwd=ROOT).decode().strip(),
              "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "cpu": next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name"))}, "datasets": {}}
    with tempfile.TemporaryDirectory(prefix="louiselm-usage-bench-") as scratch:
        root = Path(scratch)
        wrapper = root / "bin"
        wrapper.mkdir()
        (wrapper / "sqlite3").symlink_to(Path(__file__).resolve())
        env = {**os.environ, "PATH": str(wrapper) + os.pathsep + os.environ["PATH"], "LOUISELM_BENCH_SQLITE": sqlite}
        datasets = [(f"synthetic-{size}", size) for size in args.sizes]
        if args.current:
            datasets.insert(0, ("current", None))
        for name, size in datasets:
            directory = root / name
            directory.mkdir(mode=0o700)
            path = directory / "turns.sqlite3"
            if size is None:
                with sqlite3.connect(f"file:{args.current.resolve()}?mode=ro", uri=True) as source, sqlite3.connect(path) as target:
                    source.backup(target)
            else:
                nvim(directory, "seed")
                fixture(path, size)
            path.chmod(0o600)
            print(f"Measuring {name}", file=sys.stderr, flush=True)
            result["datasets"][name] = measure(directory, sqlite, env, args.profile)
        baseline = []
        for _ in range(REPEATS):
            _, ms = elapsed(lambda: run([sqlite, "-batch", "-init", "/dev/null", ":memory:", "SELECT 1;"]))
            baseline.append(ms)
        result["sqlite_process_select1_ms"] = distribution(baseline)
        result["cat_empty_process_ms"] = distribution([
            elapsed(lambda: run(["cat", "/dev/null"]))[1] for _ in range(REPEATS)])
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    if Path(sys.argv[0]).name == "sqlite3":
        # Capture once via PATH, then measure with the genuine executable. Never
        # replace vim.system or introspect private Lua functions.
        sql = sys.stdin.buffer.read()
        target = Path(sys.argv[-1]).parent / os.environ["LOUISELM_BENCH_CASE"]
        target.with_suffix(".sql").write_bytes(sql)
        response = subprocess.run([os.environ["LOUISELM_BENCH_SQLITE"], *sys.argv[1:]], input=sql, capture_output=True, timeout=10)
        target.with_suffix(".out").write_bytes(response.stdout)
        sys.stdout.buffer.write(response.stdout)
        sys.stderr.buffer.write(response.stderr)
        sys.exit(response.returncode)
    main()

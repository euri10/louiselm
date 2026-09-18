#!/usr/bin/env python3
"""Print bounded public CI evidence for completed unsplit or split workflows."""

import datetime as dt
import json
import math
import re
import subprocess
import sys


def api(path, raw=False):
    value = subprocess.check_output([
        "gh", "api", "--allow-escape-sequences", "repos/euri10/louiselm/" + path,
    ], text=True)
    return value if raw else json.loads(value)


def elapsed(start, end):
    return (dt.datetime.fromisoformat(end.replace("Z", "+00:00"))
            - dt.datetime.fromisoformat(start.replace("Z", "+00:00"))).total_seconds()


def collect(run_id):
    run = api("actions/runs/" + run_id)
    assert run["status"] == "completed", (run_id, run["status"])
    jobs = api("actions/runs/" + run_id + "/jobs?per_page=100")["jobs"]
    skills = [job for job in jobs if job["name"].startswith("cargo (skills-core")]
    # One baseline job, two initial probe jobs, or two jobs plus the aggregate.
    assert len(skills) in (1, 2, 3), [job["name"] for job in skills]
    selected = [job for job in skills if len(skills) <= 2 or job["name"] != "cargo (skills-core)"]
    assert len(selected) in (1, 2), [job["name"] for job in selected]
    result = {
        "run": int(run_id), "url": run["html_url"], "head_sha": run["head_sha"],
        "event": run["event"], "conclusion": run["conclusion"],
        "all_gates": [{"name": job["name"], "conclusion": job["conclusion"]} for job in jobs],
        "queue_seconds": elapsed(run["created_at"], min(job["started_at"] for job in selected)),
        "skills_start_skew_seconds": elapsed(min(job["started_at"] for job in selected),
                                            max(job["started_at"] for job in selected)),
        "skills_wall_seconds": elapsed(min(job["started_at"] for job in selected),
                                       max(job["completed_at"] for job in skills)),
        "skills_runner_seconds": sum(elapsed(job["started_at"], job["completed_at"]) for job in skills),
        "skills_rounded_runner_minutes": sum(math.ceil(elapsed(job["started_at"], job["completed_at"]) / 60)
                                             for job in skills),
        "required_status": [{"name": job["name"], "conclusion": job["conclusion"],
                             "seconds": elapsed(job["started_at"], job["completed_at"])}
                            for job in skills if job not in selected],
        "jobs": [],
    }
    for job in selected:
        log = api(f"actions/jobs/{job['id']}/logs", raw=True)
        lines = [re.sub(r"\x1b\[[0-9;]*m", "", line) for line in log.splitlines()]
        evidence = {"cargo": [], "cache": [], "resources": [], "scenarios": []}
        resource_group = resource_output = False
        scenario = None
        for line in lines:
            message = line.partition(" ")[2]
            if "##[group]" in message:
                resource_group = "Run uname -r" in message
                resource_output = False
            elif "##[endgroup]" in message:
                resource_output = resource_group
            elif resource_output:
                evidence["resources"].append(message)
            if "Finished `" in message and " profile " in message:
                evidence["cargo"].append(line)
            if any(text in message for text in ["Cache Size:", "Cache restored", "Cache saved",
                                                "Cache hit occurred", "Cache not found", "Cache size of"]):
                evidence["cache"].append(line)
            if message.startswith("Privileged scenario: "):
                assert scenario is None, scenario
                scenario = {"name": message.removeprefix("Privileged scenario: ")}
            match = re.fullmatch(r"Privileged scenario elapsed: ([0-9.]+) seconds \(user ([0-9.]+), sys ([0-9.]+)\)", message)
            if match:
                assert scenario is not None, message
                scenario.update(zip(["wall_seconds", "user_seconds", "sys_seconds"], map(float, match.groups())))
                evidence["scenarios"].append(scenario)
                scenario = None
        assert scenario is None, scenario
        if job["conclusion"] == "success":
            if len(selected) == 2:
                assert len(evidence["scenarios"]) == (9 if "lifecycle" in job["name"] else 12)
            assert len(evidence["cargo"]) >= 2, job["name"]
        steps = [{key: step[key] for key in ["name", "conclusion", "started_at", "completed_at"]}
                 | {"seconds": elapsed(step["started_at"], step["completed_at"])}
                 for step in job["steps"] if step["status"] == "completed" and step["conclusion"] != "skipped"]
        result["jobs"].append({
            "id": job["id"], "name": job["name"], "conclusion": job["conclusion"],
            "seconds": elapsed(job["started_at"], job["completed_at"]),
            "steps": steps, "evidence": evidence,
        })
    return result


if __name__ == "__main__":
    print(json.dumps([collect(run_id) for run_id in sys.argv[1:]], indent=2))

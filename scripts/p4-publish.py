#!/usr/bin/env python3
"""Publish generated paths from a dedicated P4Forge workspace in one numbered CL.

Requires a separately provisioned write account. Never invoked by the current
read-only Linux Agent. Install on the future publishing runner as a trusted tool.
"""
import io
import marshal
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys


def p4(*args, form=None, empty_ok=False):
    result = subprocess.run(
        ["p4", "-G", *args],
        input=None if form is None else marshal.dumps({k.encode(): v.encode() for k, v in form.items()}, 0),
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    stream, records = io.BytesIO(result.stdout), []
    while stream.tell() < len(result.stdout):
        raw = marshal.load(stream)
        records.append({k.decode() if isinstance(k, bytes) else k:
                        v.decode(errors="replace") if isinstance(v, bytes) else v
                        for k, v in raw.items()})
    errors = [r for r in records if r.get("code") == "error"]
    allowed_empty = empty_ok and errors and all(int(r.get("generic", -1)) == 17 for r in errors)
    if (errors or result.returncode) and not allowed_empty:
        message = "; ".join(str(r.get("data", "P4 command failed")) for r in errors)
        raise RuntimeError((message or result.stderr.decode(errors="replace") or "P4 command failed")[:4000])
    return [r for r in records if r.get("code") != "error"]


def publish(paths, env=os.environ, run=p4):
    for key in ("P4PORT", "P4USER", "P4CLIENT", "P4TICKETS", "P4FORGE_CHANGELIST"):
        if not env.get(key):
            raise ValueError(f"{key} is required")
    client = env["P4CLIENT"]
    if not re.fullmatch(r"p4forge_[A-Za-z0-9_]+", client):
        raise ValueError("publishing requires a dedicated p4forge_ workspace")
    if not env["P4FORGE_CHANGELIST"].isdigit() or int(env["P4FORGE_CHANGELIST"]) < 1:
        raise ValueError("P4FORGE_CHANGELIST must be a positive submitted changelist")
    if not paths:
        raise ValueError("provide explicit generated paths")
    for path in paths:
        base = path.removesuffix("/...")
        if (not path.endswith("/...") or base in ("", ".", "..") or PurePosixPath(base).is_absolute()
                or ".." in PurePosixPath(base).parts or base.startswith("-")
                or any(c in base for c in "\\:*@#%\n\r\0") or "..." in base):
            raise ValueError("publish paths must stay in an explicit generated directory")
        if not Path(base).is_dir() or not Path(base).resolve().is_relative_to(Path.cwd().resolve()):
            raise ValueError("generated directories must exist inside the workspace")
    if run("opened", "-C", client, empty_ok=True):
        raise RuntimeError("workspace already has open files; refusing to include unrelated changes")
    created = run("change", "-i", form={
        "Change": "new", "Client": client, "User": env["P4USER"],
        "Description": f"ci: generated outputs from CL {env['P4FORGE_CHANGELIST']}",
    })
    message = "\n".join(str(r.get("data", "")) for r in created)
    match = re.search(r"\bChange ([0-9]+) created\.", message)
    if not match:
        raise RuntimeError("cannot identify created changelist; inspect P4 pending changes")
    change, completed = match.group(1), False
    try:
        run("reconcile", "-a", "-e", "-d", "-c", change, *paths, empty_ok=True)
        opened = run("opened", "-c", change, empty_ok=True)
        if not opened:
            run("change", "-d", change)
            completed = True
            print("Generated files unchanged; no submit.")
            return None
        result = run("submit", "-c", change)
        completed = True
        submitted = next((str(r["submittedChange"]) for r in result if "submittedChange" in r), change)
        print(f"P4FORGE_PUBLISHED_CHANGELIST={submitted}")
        return submitted
    finally:
        if not completed:
            # A failed/ambiguous submit may already be committed. Only clean our
            # own pending CL and preserve generated workspace bytes (-k).
            try:
                status = run("change", "-o", change)
                if any(r.get("Status") == "pending" for r in status):
                    run("revert", "-k", "-c", change, f"//{client}/...", empty_ok=True)
                    run("change", "-d", change)
            except Exception as error:
                print(f"Pending CL {change} cleanup needs attention: {error}", file=sys.stderr)


if __name__ == "__main__":
    try:
        publish(sys.argv[1:])
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)

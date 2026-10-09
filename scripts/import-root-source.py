#!/usr/bin/env python3
"""Stage an explicit source folder, then import it into an empty P4Forge stream.

The original folder is never modified. Credentials are supplied through P4TICKETS.
The manifest records included files and exclusions without reading secret files.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys

from importlib.util import module_from_spec, spec_from_file_location

spec = spec_from_file_location("p4_publish", Path(__file__).with_name("p4-publish.py"))
module = module_from_spec(spec)
spec.loader.exec_module(module)
p4 = module.p4

EXCLUDED_DIRS = {".lore", ".git", ".idea", ".codegraph", "target", "node_modules",
                 "Library", "Temp", "obj", "__pycache__", ".cache", ".aws", ".ssh"}
EXCLUDED_NAMES = {".DS_Store", ".lore-ci.toml", ".loreignore", ".p4config", ".p4tickets",
                  ".p4trust", "lore-ci.md", "Game_Server_CICD_Pipeline.md"}
SECRET_SUFFIXES = {".p12", ".pfx", ".key", ".pem", ".jks", ".keystore", ".mobileprovision"}


def reason(path):
    if any(part in EXCLUDED_DIRS for part in path.parts):
        return "VCS metadata, editor state or build cache"
    if path.name in EXCLUDED_NAMES:
        return "machine-local or superseded Lore configuration/documentation"
    if path.name == ".env" or path.name.startswith(".env."):
        return "environment file (credentials are configured separately)"
    if path.suffix.lower() in SECRET_SUFFIXES or path.name in {"credentials", "id_rsa", "id_ed25519"}:
        return "credential/key material"
    return None


def stage(source, destination):
    if destination.exists():
        raise ValueError("staging destination already exists; use a fresh directory")
    included, excluded, casefolded = [], [], set()
    destination.mkdir(parents=True)
    for directory, dirs, files in os.walk(source, followlinks=False):
        relative = Path(directory).relative_to(source)
        for name in list(dirs):
            path = relative / name
            why = reason(path)
            if why or Path(directory, name).is_symlink():
                excluded.append({"path": path.as_posix() + "/", "reason": why or "directory symlink"})
                dirs.remove(name)
        for name in sorted(files):
            path = relative / name
            why = reason(path)
            original = source / path
            if why or original.is_symlink():
                excluded.append({"path": path.as_posix(), "reason": why or "symlink requires review"})
                continue
            key = path.as_posix().casefold()
            if key in casefolded or any(c in key for c in "@#%*\n\r") or "..." in key:
                raise ValueError(f"path cannot be imported safely on Windows/P4: {path}")
            casefolded.add(key)
            target = destination / path
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original, target)
            data = target.read_bytes()
            included.append({"path": path.as_posix(), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
    return {"source": str(source), "files": included, "excluded": excluded,
            "file_count": len(included), "bytes": sum(f["bytes"] for f in included)}


def submit(destination, manifest, stream):
    if stream != "//streams/root":
        raise ValueError("this migration is scoped to //streams/root")
    info = p4("info")
    if not any(r.get("ServerID") == "p4forge-dev" for r in info):
        raise ValueError("expected the dedicated p4forge-dev server")
    if p4("files", stream + "/...", empty_ok=True):
        raise ValueError("destination already contains files; refusing a second import")
    client = "p4forge_root_import"
    if p4("clients", "-e", client):
        raise ValueError("import client already exists; inspect its pending work")
    if not p4("streams", "-F", f"Stream={stream}"):
        p4("stream", "-i", form={"Stream": stream, "Owner": os.environ["P4USER"],
            "Name": "Root", "Parent": "none", "Type": "mainline", "ParentView": "inherit", "Paths0": "share ..."})
    p4("client", "-i", form={"Client": client, "Owner": os.environ["P4USER"],
        "Root": str(destination), "Stream": stream, "LineEnd": "unix",
        "Options": "noallwrite noclobber nocompress unlocked nomodtime rmdir"})
    os.environ["P4CLIENT"] = client
    os.chdir(destination)
    os.environ["PWD"] = str(destination)
    created = p4("change", "-i", form={"Change": "new", "Client": client,
        "User": os.environ["P4USER"], "Description": "Import root source into Perforce; exclude credentials, Lore metadata and build caches"})
    import re
    change = re.search(r"Change (\d+) created", str(created)).group(1)
    # Numbered CL plus an exact manifest keeps unrelated files out of this submit.
    for start in range(0, len(manifest["files"]), 100):
        p4("add", "-c", change, *[str(destination / f["path"]) for f in manifest["files"][start:start + 100]])
    opened = p4("opened", "-c", change)
    if len(opened) != manifest["file_count"]:
        raise ValueError(f"opened file count mismatch; inspect pending CL {change}")
    result = p4("submit", "-c", change)
    submitted = next((r["submittedChange"] for r in result if "submittedChange" in r), change)
    p4("client", "-d", client)
    return int(submitted)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--stage", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--submit", action="store_true")
    args = parser.parse_args()
    source, destination, report = args.source.resolve(), args.stage.resolve(), args.manifest.resolve()
    if source == destination or source in destination.parents or destination in source.parents:
        raise ValueError("staging must be outside the original source")
    if report.exists():
        manifest = json.loads(report.read_text())
        if manifest["source"] != str(source):
            raise ValueError("manifest belongs to a different source")
        for f in manifest["files"]:
            if hashlib.sha256((destination / f["path"]).read_bytes()).hexdigest() != f["sha256"]:
                raise ValueError("staged source changed since inspection")
    else:
        manifest = stage(source, destination)
        report.parent.mkdir(parents=True, exist_ok=True)
        report.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    if args.submit:
        manifest["submitted_changelist"] = submit(destination, manifest, "//streams/root")
        report.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({k: v for k, v in manifest.items() if k not in {"files", "excluded"}}, ensure_ascii=False))
    print(f"Exclusions: {len(manifest['excluded'])}; manifest: {report}")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)

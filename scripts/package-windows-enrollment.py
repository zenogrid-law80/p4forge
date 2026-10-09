#!/usr/bin/env python3
"""Prepare the authorized Windows host's private enrollment archive; never publish it publicly."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parent.parent
os.umask(0o077)
public = ROOT / '.local/releases/p4forge-windows-x64-preview.zip'
token = (ROOT / '.local/windows-desktop-6n6u52l-token').read_text().strip()
if not re.fullmatch(r'[A-Za-z0-9_-]{43}', token):
    raise SystemExit('Invalid Windows Agent credential file.')
tickets = ROOT / '.local/p4tickets'
environment = {key: value for key, value in os.environ.items() if not key.startswith('P4')}
environment.update(P4TICKETS=str(tickets), P4ENVIRO='/dev/null', P4IGNORE='')
status = subprocess.run(
    ['/opt/homebrew/bin/p4', '-ztag', '-p', 'p4forge.zenogrid.co.kr:1666', '-u', 'p4forge', '-C', 'utf8', 'login', '-s'],
    env=environment, capture_output=True, text=True, timeout=15,
)
remaining = re.search(r'^\.\.\. TicketExpiration (\d+)$', status.stdout, re.MULTILINE)
if status.returncode or not remaining or int(remaining[1]) < 1200:
    raise SystemExit('Refresh the authorized read ticket before packaging; at least 20 minutes is required.')
now = datetime.datetime.now(datetime.timezone.utc)
config = dict(
    package_revision='p4-unicode-v3',
    computer='DESKTOP-6N6U52L',
    agent_id='b1504676-d7a1-4d89-b4e7-5eb06acffe6c',
    server='https://p4forge.zenogrid.co.kr:8444/',
    p4port='p4forge.zenogrid.co.kr:1666', p4user='p4forge', p4charset='utf8',
    expires_at=(now + datetime.timedelta(seconds=int(remaining[1]) - 300)).isoformat(),
)
with zipfile.ZipFile(public) as archive:
    files = {}
    for name in archive.namelist():
        path = Path(name)
        if len(path.parts) != 2 or not re.fullmatch(r'[A-Za-z0-9._-]+', path.name):
            raise SystemExit('Invalid source package entry.')
        files[path.name] = archive.read(name)
    for line in files.pop('SHA256SUMS').decode().splitlines():
        expected, name = line.split('  ', 1)
        if hashlib.sha256(files[name]).hexdigest() != expected:
            raise SystemExit('Source package checksum mismatch.')
files['enrollment.json'] = (json.dumps(config, indent=2) + '\n').encode()
files['windows-smoke.toml'] = files['windows-smoke.toml'].replace(
    b'SOURCE_ROOT_UUID', b'09a5f79b-96e7-47be-a022-92a6c0101210'
)
files['SHA256SUMS'] = ''.join(
    f'{hashlib.sha256(data).hexdigest()}  {name}\n' for name, data in sorted(files.items())
).encode()
# Private credentials are intentionally absent from the public ZIP and its manifest.
files['agent-token'] = token.encode()
entries = {}
for line in tickets.read_text().splitlines():
    match = re.fullmatch(r'([^=]+)=p4forge:([0-9a-fA-F]{32})', line)
    if not match:
        raise SystemExit('Unexpected ticket file entry; refusing to bundle unrelated credentials.')
    entries[match[1]] = match[2]
if len(set(entries.values())) != 1:
    raise SystemExit('Conflicting source tickets; validate the target server before packaging.')
# Preserve server-reported keys and include the configured Windows connection name.
ticket = next(iter(entries.values()))
for alias in ('p4forge.zenogrid.co.kr:1666', '10.48.100.171:1666'):
    entries[alias] = ticket
files['p4tickets'] = ''.join(f'{server}=p4forge:{value}\r\n' for server, value in entries.items()).encode('ascii')
directory = ROOT / '.local/enrollment'
directory.mkdir(mode=0o700, exist_ok=True)
directory.chmod(0o700)
target = directory / 'p4forge-desktop-6n6u52l-setup.zip'
pending = target.with_suffix('.zip.next')
with zipfile.ZipFile(pending, 'w', zipfile.ZIP_DEFLATED) as archive:
    for name, data in sorted(files.items()):
        archive.writestr('p4forge-desktop-6n6u52l-setup/' + name, data)
pending.chmod(0o600)
os.replace(pending, target)
print('Prepared private installation ZIP for DESKTOP-6N6U52L.')
print('Installation credential deadline (UTC):', config['expires_at'])
print('SHA-256:', hashlib.sha256(target.read_bytes()).hexdigest())

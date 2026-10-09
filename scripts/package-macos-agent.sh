#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --locked --release -p p4forge-agent
python3 - <<'PY'
import hashlib, pathlib, platform, shutil, tarfile
name = 'p4forge-macos-' + platform.machine() + '-preview'
base = pathlib.Path('.local/releases')
package = base / name
package.mkdir(parents=True, exist_ok=True)
for source in pathlib.Path('deploy/macos').iterdir():
    if source.is_file(): shutil.copy2(source, package / source.name)
shutil.copy2('target/release/p4forge-agent', package / 'p4forge-agent')
(package / 'SHA256SUMS').write_text(''.join(
    f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n'
    for p in sorted(package.iterdir()) if p.is_file() and p.name != 'SHA256SUMS'))
archive = base / (name + '.tar.gz')
with tarfile.open(archive, 'w:gz') as tar: tar.add(package, arcname=name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix('.gz.sha256').write_text(f'{digest}  {archive.name}\n')
print(archive.resolve())
print('SHA-256:', digest)
PY

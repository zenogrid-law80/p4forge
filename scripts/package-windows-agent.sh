#!/bin/sh
# Cross-compile using the installed Rust Windows GNU target and MinGW toolchain.
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
cargo build --locked --release -p p4forge-agent --target x86_64-pc-windows-gnu
python3 - <<'PY'
import hashlib
from pathlib import Path
import zipfile

name = 'p4forge-windows-x64-preview'
output = Path('.local/releases')
output.mkdir(parents=True, exist_ok=True)
files = {p.name: p.read_bytes() for p in Path('deploy/windows').iterdir() if p.is_file()}
files['p4forge-agent.exe'] = Path('target/x86_64-pc-windows-gnu/release/p4forge-agent.exe').read_bytes()
files['SHA256SUMS'] = ''.join(f'{hashlib.sha256(data).hexdigest()}  {filename}\n' for filename,data in sorted(files.items())).encode()
archive = output / f'{name}.zip'
with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as bundle:
    for filename, data in sorted(files.items()):
        bundle.writestr(f'{name}/{filename}', data)
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix('.zip.sha256').write_text(f'{checksum}  {archive.name}\n')
print(f'Windows package: {archive.resolve()} ({archive.stat().st_size} bytes)')
print(f'SHA-256: {checksum}')
PY

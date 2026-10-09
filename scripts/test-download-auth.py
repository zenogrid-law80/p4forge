#!/usr/bin/env python3
"""Exercise the real Nginx download rules with a synthetic authorization upstream."""
from pathlib import Path
import re
import subprocess
import tempfile
import uuid

root = Path(__file__).resolve().parent.parent
name = 'p4forge-download-auth-' + uuid.uuid4().hex[:12]
private_name = 'p4forge-desktop-6n6u52l-setup.zip'
with tempfile.TemporaryDirectory(prefix='p4forge-download-auth-') as directory:
    tmp = Path(directory)
    private = tmp / 'private'
    public = tmp / 'downloads'
    private.mkdir()
    public.mkdir()
    (private / private_name).write_bytes(b'private-test-fixture')
    (public / 'p4forge-windows-x64-preview.zip').write_bytes(b'public-test-fixture')
    (public / 'p4forge-windows-self-patch-bootstrap.zip').write_bytes(b'bootstrap-test-fixture')
    (public / 'p4forge-windows-self-patch-bootstrap.zip.sha256').write_bytes(b'bootstrap-checksum-fixture')
    config = (root / 'deploy/downloads.nginx.conf').read_text()
    config = config.replace('http://server:8088/api/agents', 'http://127.0.0.1:8081/api/agents')
    config = config.replace('http {', '''http {
        server { listen 8081; location = /api/agents {
            if ($http_cookie = "role=admin") { return 204; }
            if ($http_cookie = "role=viewer") { return 403; }
            if ($http_cookie = "role=unavailable") { return 503; }
            return 401;
        } }
    ''', 1)
    path = tmp / 'nginx.conf'
    path.write_text(config)
    try:
        subprocess.run([
            'docker', 'run', '--rm', '-d', '--name', name, '--network', 'none',
            '--read-only', '--tmpfs', '/tmp', '--user', '101:101', '--cap-drop', 'ALL',
            '--security-opt', 'no-new-privileges:true',
            '-v', f'{path}:/etc/nginx/nginx.conf:ro',
            '-v', f'{private}:/private:ro', '-v', f'{public}:/srv/downloads:ro',
            '--entrypoint', 'nginx', 'nginx:1.28-alpine', '-g', 'daemon off;',
        ], check=True, stdout=subprocess.DEVNULL)
        cases = [
            ('private anonymous', '', '/downloads/' + private_name, 401, None),
            ('private invalid session', 'role=invalid', '/downloads/' + private_name, 401, None),
            ('private non-admin', 'role=viewer', '/downloads/' + private_name, 403, None),
            ('private admin', 'role=admin', '/downloads/' + private_name, 200, b'private-test-fixture'),
            ('private auth failure', 'role=unavailable', '/downloads/' + private_name, 500, None),
            ('internal endpoint', 'role=admin', '/_installer_admin', 404, None),
            ('unlisted file', 'role=admin', '/downloads/agent-token', 404, None),
            ('public preview', '', '/downloads/p4forge-windows-x64-preview.zip', 200, b'public-test-fixture'),
            ('public self-patch bootstrap', '', '/downloads/p4forge-windows-self-patch-bootstrap.zip', 200, b'bootstrap-test-fixture'),
            ('public bootstrap checksum', '', '/downloads/p4forge-windows-self-patch-bootstrap.zip.sha256', 200, b'bootstrap-checksum-fixture'),
        ]
        for label, cookie, url, expected, body in cases:
            response = subprocess.run([
                'docker', 'exec', name, 'wget', '-S', '-O', '/tmp/response',
                '--header', 'Cookie: ' + cookie, 'http://127.0.0.1:8080' + url,
            ], capture_output=True)
            statuses = re.findall(rb'HTTP/1\.1 (\d+)', response.stderr)
            assert statuses and int(statuses[-1]) == expected, (label, response.stderr.decode())
            if body is not None:
                result = subprocess.run(['docker', 'exec', name, 'cat', '/tmp/response'], check=True, capture_output=True)
                assert result.stdout == body, label
            print('PASS:', label, expected)
    finally:
        subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

#!/usr/bin/env python3
"""Root-only pre-start test. Refuses to interrupt a running Agent service."""
import fcntl
import os
import pathlib
import signal
import subprocess
import time

AGENT = '/Library/P4Forge/bin/p4forge-agent'
SERVICE = 'system/kr.co.zenogrid.p4forge.agent'
assert os.geteuid() == 0, 'administrator required'
assert subprocess.run(['/bin/launchctl', 'print', SERVICE], stdout=subprocess.DEVNULL,
                      stderr=subprocess.DEVNULL).returncode != 0, 'stop the Agent first'

def build_identity():
    os.setgroups([])
    os.setgid(10003)
    os.setuid(10003)
    os.setsid()

def cleanup():
    subprocess.run([AGENT, '--macos-kill-builds'], check=True, timeout=5)

cleanup()  # Regression: an empty build UID must not kill the helper itself.
child = subprocess.Popen(['/bin/sleep', '60'], preexec_fn=build_identity)
try:
    cleanup()
    assert child.wait(timeout=3) == -signal.SIGKILL, 'detached child survived'
finally:
    if child.poll() is None:
        child.kill()
        child.wait()

# Test EOF cleanup and lock retention independently of network/Perforce.
lock_path = pathlib.Path('/Library/P4Forge/state/watchdog-test.lock')
lock = lock_path.open('a+b')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
watchdog = subprocess.Popen([AGENT, '--macos-watchdog', str(lock.fileno())],
                           stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           pass_fds=(lock.fileno(),))
assert watchdog.stdout.read(1) == b'R', 'watchdog not ready'
lock.close()
probe = lock_path.open('a+b')
try:
    fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
    raise AssertionError('watchdog lost state lock before cleanup')
except BlockingIOError:
    pass
child = subprocess.Popen(['/bin/sleep', '60'], preexec_fn=build_identity)
watchdog.stdin.close()  # The kernel also closes this pipe on Supervisor SIGKILL.
assert watchdog.wait(timeout=5) == 0
assert child.wait(timeout=3) == -signal.SIGKILL, 'EOF cleanup missed detached child'
fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
probe.close()
lock_path.unlink()
print('PASS: empty cleanup, detached child termination, watchdog EOF and state lock')

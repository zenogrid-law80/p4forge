"""Stop an external operation if its executor disappears, including SIGKILL/reboot."""
import os
import select
import signal
import subprocess
import sys


def main():
    owner_pipe = int(sys.argv[1])
    stopping = False

    def stop(*_):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    child = subprocess.Popen(sys.argv[2:], start_new_session=True)
    try:
        while child.poll() is None:
            if stopping or select.select([owner_pipe], [], [], 0.2)[0]:
                # The executor writes nothing: readability means its pipe closed.
                return 143
        return child.returncode
    finally:
        try:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        except ProcessLookupError:
            pass
        child.wait()
        os.close(owner_pipe)


if __name__ == "__main__":
    sys.exit(main())

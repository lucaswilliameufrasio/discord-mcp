"""Test-only supervisor. Not a service manager or adoption verifier."""
import ctypes
import os
import signal
import subprocess
import sys
import time

stop = False


def on_term(_sig, _frame):
    global stop
    stop = True


signal.signal(signal.SIGTERM, on_term)
parent = os.getpid()


def parent_death():
    if os.getppid() != parent:
        os._exit(3)
    if ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
        os._exit(4)
    if os.getppid() != parent:
        os._exit(5)


child = subprocess.Popen([sys.argv[1], "serve"], stdin=subprocess.DEVNULL, preexec_fn=parent_death)
try:
    while child.poll() is None:
        if stop:
            child.terminate()
            try:
                sys.exit(child.wait(timeout=20))
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
                sys.exit(1)
        time.sleep(0.02)
    sys.exit(child.returncode)
finally:
    if child.poll() is None:
        child.kill()
        child.wait()

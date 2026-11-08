#!/usr/bin/env python3
"""Linux-only lifecycle fixture; it does not run or configure discord-mcp."""
import ctypes
import os
import select
import signal
import socket
import struct
import subprocess
import sys
import time


def worker(expected_parent):
    control = socket.socket(fileno=0)
    # Fixture only: parent death kills this process immediately, without drain.
    if os.getppid() != expected_parent:
        return 2
    if ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
        return 3
    if os.getppid() != expected_parent:
        return 4
    draining = False

    def on_term(_signum, _frame):
        nonlocal draining
        draining = True

    signal.signal(signal.SIGTERM, on_term)
    control.sendall(b"R")  # One synthetic request is already in flight.
    announced = False
    while True:
        if draining and not announced:
            control.sendall(b"D")
            announced = True
        ready, _, _ = select.select([control], [], [], 0.05)
        if not ready:
            continue
        command = control.recv(1)
        if not command:
            return 5
        if command == b"N" and draining:
            control.sendall(b"X")  # Reject new work.
        elif command == b"F" and draining:
            control.sendall(b"F")  # Existing synthetic work completed.
            return 0
        else:
            return 6


def supervisor():
    controller = socket.socket(fileno=0)
    local, remote = socket.socketpair()
    child = subprocess.Popen([sys.executable, "-I", __file__, "worker", str(os.getpid())],
                             stdin=remote, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    remote.close()
    local.settimeout(5)
    terminating = False

    def on_term(_signum, _frame):
        nonlocal terminating
        terminating = True

    signal.signal(signal.SIGTERM, on_term)
    try:
        if local.recv(1) != b"R":
            return 2
        controller.sendall(struct.pack("!I", child.pid))
        deadline = None
        while True:
            if terminating and deadline is None:
                child.send_signal(signal.SIGTERM)
                deadline = time.monotonic() + 1.0
            if deadline is not None and time.monotonic() >= deadline:
                child.kill()
                child.wait(timeout=2)
                controller.sendall(b"T")
                return 1
            ready, _, _ = select.select([controller, local], [], [], 0.05)
            if controller in ready:
                data = controller.recv(1)
                if not data:
                    return 3
                local.sendall(data)
            if local in ready:
                data = local.recv(1)
                if not data:
                    return 4
                controller.sendall(data)
                if data == b"F":
                    return child.wait(timeout=2)
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=2)
        local.close()
        controller.close()


def run_case(case):
    controller, child_end = socket.socketpair()
    controller.settimeout(5)
    parent = subprocess.Popen([sys.executable, "-I", __file__, "supervisor"],
                              stdin=child_end, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    child_end.close()
    pidfd = None
    try:
        raw = b""
        while len(raw) < 4:
            chunk = controller.recv(4 - len(raw))
            if not chunk:
                raise RuntimeError("supervisor exited before readiness")
            raw += chunk
        pidfd = os.pidfd_open(struct.unpack("!I", raw)[0])
        if case == "crash":
            parent.kill()
            assert parent.wait(timeout=5) == -signal.SIGKILL
            assert select.select([pidfd], [], [], 5)[0], "worker survived parent death"
            return
        parent.send_signal(signal.SIGTERM)
        assert controller.recv(1) == b"D"
        controller.sendall(b"N")
        assert controller.recv(1) == b"X"
        if case == "drain":
            controller.sendall(b"F")
            assert controller.recv(1) == b"F"
            assert parent.wait(timeout=5) == 0
        else:
            assert controller.recv(1) == b"T"
            assert parent.wait(timeout=5) == 1
        assert select.select([pidfd], [], [], 5)[0], "worker did not exit"
    finally:
        if parent.poll() is None:
            parent.kill()
            parent.wait(timeout=5)
        if pidfd is not None:
            try:
                signal.pidfd_send_signal(pidfd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(pidfd)
        controller.close()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "worker":
        sys.exit(worker(int(sys.argv[2])))
    if len(sys.argv) > 1 and sys.argv[1] == "supervisor":
        sys.exit(supervisor())
    for scenario in ("drain", "timeout", "crash"):
        run_case(scenario)
        print(f"PASS: {scenario}")

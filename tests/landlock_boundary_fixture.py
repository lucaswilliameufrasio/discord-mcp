"""Opt-in Linux Landlock experiment; not an application security boundary."""
import contextlib
import ctypes
import os
import platform
import socket
import sqlite3
import subprocess
import sys
import tempfile


def restrict_writes():
    if platform.machine() != "x86_64":
        raise RuntimeError("fixture requires x86_64 syscall numbers")
    libc = ctypes.CDLL(None, use_errno=True)
    version = libc.syscall(444, 0, 0, 1)
    if version < 3:
        raise RuntimeError("Landlock ABI >= 3 unavailable")

    class Ruleset(ctypes.Structure):
        _fields_ = [("handled_access_fs", ctypes.c_uint64)]

    # Deny handled write/create/remove/refer/truncate operations everywhere.
    # No allow rules are installed. Read access is outside this experiment.
    rights = (1 << 1) | sum(1 << bit for bit in range(4, 15))
    ruleset = Ruleset(rights)
    fd = libc.syscall(444, ctypes.byref(ruleset), ctypes.sizeof(ruleset), 0)
    if fd < 0:
        raise OSError(ctypes.get_errno(), "landlock_create_ruleset failed")
    try:
        if libc.prctl(38, 1, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "PR_SET_NO_NEW_PRIVS failed")
        if libc.syscall(446, fd, 0) != 0:
            raise OSError(ctypes.get_errno(), "landlock_restrict_self failed")
    finally:
        os.close(fd)


def restricted_client(database, control_fd, broker_fd):
    control = socket.socket(fileno=control_fd)
    broker = socket.socket(fileno=broker_fd)
    control.settimeout(5)
    broker.settimeout(5)
    restrict_writes()

    def denied():
        # Read remains possible; write and delete must be denied.
        fd = os.open(database, os.O_RDONLY)
        os.close(fd)
        for action in (lambda: os.open(database, os.O_RDWR), lambda: os.unlink(database)):
            try:
                result = action()
            except PermissionError:
                continue
            if isinstance(result, int):
                os.close(result)
            raise RuntimeError("Landlock failed to deny a direct write/delete")

    denied()
    control.sendall(f"DENIED {os.getpid()}".encode())
    broker.sendall(b"B")
    if broker.recv(1) != b"T":
        raise RuntimeError("broker did not start transaction")
    control.sendall(b"INFLIGHT")
    if control.recv(1) != b"K" or broker.recv(1) != b"":
        raise RuntimeError("broker death was not observed")
    denied()
    control.sendall(b"DEAD_DENIED")


def stop(process):
    if process.poll() is None:
        process.kill()
    process.communicate(timeout=5)


def run():
    with open("/proc/self/cgroup", encoding="ascii") as source:
        lines = source.read().splitlines()
    name = os.path.basename(lines[0][3:]) if len(lines) == 1 and lines[0].startswith("0::/") else ""
    if not name.startswith("discord-mcp-cgroup-drill-") or not name.endswith(".scope"):
        raise RuntimeError("requires an owned transient test scope")
    sibling = os.path.join(os.path.dirname(__file__), "access_boundary_fixture.py")
    with contextlib.ExitStack() as stack:
        scratch = stack.enter_context(tempfile.TemporaryDirectory(prefix="landlock-boundary-", dir="/tmp/opencode"))
        database = os.path.join(scratch, "synthetic.db")
        with sqlite3.connect(database) as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
        broker_end, client_end = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        control, child_control = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        gate, gate_peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        for sock in (broker_end, client_end, control, child_control, gate, gate_peer):
            stack.callback(sock.close)
        control.settimeout(5)
        gate.settimeout(5)
        service = subprocess.Popen(
            [sys.executable, sibling, "broker", database, str(broker_end.fileno())],
            pass_fds=(broker_end.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, service)
        client = subprocess.Popen(
            [sys.executable, __file__, "client", database, str(child_control.fileno()), str(client_end.fileno())],
            pass_fds=(child_control.fileno(), client_end.fileno()), stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, client)
        guard = subprocess.Popen(
            [sys.executable, sibling, "closed_gate", str(gate_peer.fileno())],
            pass_fds=(gate_peer.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, guard)
        broker_end.close()
        client_end.close()
        child_control.close()
        gate_peer.close()
        message = control.recv(128).decode().split()
        if len(message) != 2 or message[0] != "DENIED" or int(message[1]) != client.pid:
            raise RuntimeError("restricted client did not deny access")
        with open(f"/proc/{client.pid}/status", encoding="ascii") as source:
            fields = dict(line.rstrip().split(":", 1) for line in source if ":" in line)
        if int(fields["Uid"].split()[1]) != os.geteuid():
            raise RuntimeError("client and broker did not share an effective UID")
        if control.recv(128) != b"INFLIGHT":
            raise RuntimeError("mediated transaction did not start")
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate failed closed")
        service.kill()
        service.wait(timeout=5)
        control.sendall(b"K")
        if control.recv(128) != b"DEAD_DENIED" or client.wait(timeout=5) != 0:
            raise RuntimeError("restriction did not survive broker death")
        with sqlite3.connect(database) as db:
            if db.execute("SELECT COUNT(*) FROM writes").fetchone() != (0,):
                raise RuntimeError("broker transaction did not roll back")
        print("same_uid_confined_direct_write_delete=denied", flush=True)
        print("broker_death_rollback_and_denial=verified", flush=True)

        # This same-UID process deliberately has no Landlock restriction. It
        # proves applying policy to just the client does not fence the host.
        direct, peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        stack.callback(direct.close)
        stack.callback(peer.close)
        direct.settimeout(5)
        outsider = subprocess.Popen(
            [sys.executable, sibling, "same_uid_writer", database, str(peer.fileno())],
            pass_fds=(peer.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, outsider)
        peer.close()
        if not direct.recv(128).startswith(b"DIRECT "):
            raise RuntimeError("unconfined writer did not start")
        direct.sendall(b"C")
        if direct.recv(128) != b"COMMITTED" or outsider.wait(timeout=5) != 0:
            raise RuntimeError("unconfined writer did not commit")
        with sqlite3.connect(database) as db:
            if db.execute("SELECT COUNT(*) FROM writes").fetchone() != (1,):
                raise RuntimeError("unconfined write missing")
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate opened without global fencing evidence")
        print("same_uid_unconfined_writer=committed", flush=True)
        print("global_fencing_unproven_admission_capture=closed", flush=True)
        gate.close()
        if guard.wait(timeout=5) != 0:
            raise RuntimeError("guard did not exit")


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "client":
        restricted_client(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]))
    elif len(sys.argv) == 1:
        run()
    else:
        raise SystemExit("invalid fixture arguments")

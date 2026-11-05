"""Opt-in mount namespace research against a synthetic SQLite file only."""
import contextlib
import os
import socket
import sqlite3
import subprocess
import sys
import tempfile


CHILD = r'''
import ctypes, os, socket, sqlite3, sys
directory, database = sys.argv[1:3]
database_fd = int(sys.argv[3])
sock = socket.socket(fileno=int(sys.argv[4]))
sock.settimeout(5)
libc = ctypes.CDLL(None, use_errno=True)
libc.mount.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_ulong, ctypes.c_char_p]
libc.umount2.argtypes = [ctypes.c_char_p, ctypes.c_int]
target = os.fsencode(directory)
if libc.mount(b"tmpfs", target, b"tmpfs", 14, b"size=1048576,mode=0700") != 0:
    raise OSError(ctypes.get_errno(), "private tmpfs mount failed")
try:
    os.open(database, os.O_RDWR)
except FileNotFoundError:
    pass
else:
    raise RuntimeError("masked path unexpectedly accessible")
sock.sendall(f"MASKED {os.getpid()}".encode())
if sock.recv(1) != b"P":
    raise RuntimeError("expected fd probe")
# Synthetic SQLite PRAGMA user_version header field. No real data is touched.
if os.pwrite(database_fd, (42).to_bytes(4, "big"), 60) != 4:
    raise RuntimeError("short synthetic write")
os.fsync(database_fd)
sock.sendall(b"FD_WRITTEN")
if sock.recv(1) != b"U":
    raise RuntimeError("expected unmount probe")
if libc.umount2(target, 0) != 0:
    sock.sendall(b"UNMOUNT_DENIED")
else:
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("INSERT INTO writes VALUES (1)")
    connection.commit()
    connection.close()
    sock.sendall(b"UNMOUNTED_COMMITTED")
os.close(database_fd)
sock.close()
'''


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
        scratch = stack.enter_context(tempfile.TemporaryDirectory(prefix="mount-boundary-", dir="/tmp/opencode"))
        database = os.path.join(scratch, "synthetic.db")
        with sqlite3.connect(database) as db:
            db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
        control, peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        gate, gate_peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        broker_client, broker_peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        for sock in (control, peer, gate, gate_peer, broker_client, broker_peer):
            stack.callback(sock.close)
        control.settimeout(5)
        gate.settimeout(5)
        broker_client.settimeout(5)
        authority = subprocess.Popen(
            [sys.executable, sibling, "closed_gate", str(gate_peer.fileno())],
            pass_fds=(gate_peer.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, authority)
        gate_peer.close()
        broker = subprocess.Popen(
            [sys.executable, sibling, "broker", database, str(broker_peer.fileno())],
            pass_fds=(broker_peer.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, broker)
        broker_peer.close()
        broker_client.sendall(b"B")
        if broker_client.recv(1) != b"T":
            raise RuntimeError("broker did not start")
        broker.kill()
        broker.wait(timeout=5)
        with sqlite3.connect(database) as db:
            if db.execute("SELECT COUNT(*) FROM writes").fetchone() != (0,):
                raise RuntimeError("broker rollback failed")
        fd = os.open(database, os.O_RDWR)
        try:
            child = subprocess.Popen(
                ["/usr/bin/unshare", "--user", "--map-root-user", "--mount",
                 "--propagation", "private", "/usr/bin/python3", "-c", CHILD,
                 scratch, database, str(fd), str(peer.fileno())],
                pass_fds=(fd, peer.fileno()), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
            )
        finally:
            os.close(fd)
        stack.callback(stop, child)
        peer.close()
        message = control.recv(128).decode().split()
        if len(message) != 2 or message[0] != "MASKED" or int(message[1]) != child.pid:
            raise RuntimeError("path masking was not demonstrated")
        with open(f"/proc/{child.pid}/status", encoding="ascii") as status:
            fields = dict(line.rstrip().split(":", 1) for line in status if ":" in line)
        if int(fields["Uid"].split()[1]) != os.geteuid():
            raise RuntimeError("namespace process does not have the same host UID")
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate did not remain closed")
        control.sendall(b"P")
        if control.recv(128) != b"FD_WRITTEN":
            raise RuntimeError("inherited descriptor probe failed")
        with sqlite3.connect(database) as db:
            if db.execute("PRAGMA user_version").fetchone() != (42,):
                raise RuntimeError("inherited descriptor write was not observed")
        control.sendall(b"U")
        result = control.recv(128)
        if result not in (b"UNMOUNTED_COMMITTED", b"UNMOUNT_DENIED"):
            raise RuntimeError("invalid unmount result")
        if child.wait(timeout=5) != 0:
            raise RuntimeError("namespace process failed")
        with sqlite3.connect(database) as db:
            expected = 1 if result == b"UNMOUNTED_COMMITTED" else 0
            if db.execute("SELECT COUNT(*) FROM writes").fetchone() != (expected,):
                raise RuntimeError("unexpected synthetic row count")
            if db.execute("PRAGMA integrity_check").fetchone() != ("ok",):
                raise RuntimeError("synthetic database integrity failed")
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate reopened without fencing evidence")
        print("broker_death_rollback=verified", flush=True)
        print("same_uid_path_masked=yes", flush=True)
        print("preopened_descriptor_write=verified", flush=True)
        print("namespace_unmount=" + ("allowed_and_sqlite_commit" if expected else "denied"), flush=True)
        print("global_fencing_unproven_admission_capture=closed", flush=True)
        gate.close()
        if authority.wait(timeout=5) != 0:
            raise RuntimeError("gate did not exit")


if __name__ == "__main__":
    if len(sys.argv) != 1:
        raise SystemExit("invalid fixture arguments")
    run()

"""Opt-in subordinate-UID broker experiment, not a production boundary."""
import contextlib
import os
import socket
import sqlite3
import subprocess
import sys
import tempfile


BROKER = r'''
import os, socket, sqlite3, sys
directory, socket_fd = sys.argv[1], int(sys.argv[2])
sock = socket.socket(fileno=socket_fd)
sock.settimeout(5)
os.makedirs(directory, mode=0o700, exist_ok=True)
connection = sqlite3.connect(os.path.join(directory, "synthetic.db"), timeout=5)
connection.execute("PRAGMA journal_mode=WAL")
connection.execute("CREATE TABLE IF NOT EXISTS writes(value INTEGER NOT NULL)")
connection.commit()
sock.sendall(f"READY {os.getpid()}".encode())
while True:
    command = sock.recv(1)
    if command == b"B":
        connection.execute("BEGIN IMMEDIATE")
        connection.execute("INSERT INTO writes VALUES (1)")
        sock.sendall(b"T")
    elif command == b"R":
        count = connection.execute("SELECT COUNT(*) FROM writes").fetchone()[0]
        integrity = connection.execute("PRAGMA integrity_check").fetchone()[0]
        sock.sendall(f"{count} {integrity}".encode())
    elif command == b"Q":
        break
    else:
        raise RuntimeError("invalid command")
connection.close()
sock.close()
'''

OUTSIDER = r'''
import os, socket, sqlite3, sys
database, socket_fd = sys.argv[1], int(sys.argv[2])
sock = socket.socket(fileno=socket_fd)
sock.settimeout(5)
connection = sqlite3.connect(database, timeout=5)
connection.execute("BEGIN IMMEDIATE")
connection.execute("INSERT INTO writes VALUES (2)")
sock.sendall(f"DIRECT {os.getpid()}".encode())
if sock.recv(1) != b"C":
    raise RuntimeError("expected commit")
connection.commit()
connection.close()
sock.sendall(b"COMMITTED")
'''

CLEANUP = r'''
import os, sys
directory = sys.argv[1]
if os.path.isdir(directory):
    for name in ("synthetic.db-wal", "synthetic.db-shm", "synthetic.db-journal", "synthetic.db"):
        try:
            os.unlink(os.path.join(directory, name))
        except FileNotFoundError:
            pass
    os.rmdir(directory)
'''


def stop(process):
    if process.poll() is None:
        process.kill()
    process.communicate(timeout=5)


def subuid_command(code, *args):
    return ["/usr/bin/unshare", "--user", "--map-auto", "--map-root-user",
            "--setgid", "1", "--setuid", "1", "/usr/bin/python3", "-c", code, *args]


def effective_uid(pid):
    with open(f"/proc/{pid}/status", encoding="ascii") as source:
        fields = dict(line.rstrip().split(":", 1) for line in source if ":" in line)
    if int(fields["CapEff"].strip(), 16) != 0:
        raise RuntimeError("test process has effective capabilities")
    return int(fields["Uid"].split()[1])


def run():
    with open("/proc/self/cgroup", encoding="ascii") as source:
        lines = source.read().splitlines()
    name = os.path.basename(lines[0][3:]) if len(lines) == 1 and lines[0].startswith("0::/") else ""
    if not name.startswith("discord-mcp-cgroup-drill-") or not name.endswith(".scope"):
        raise RuntimeError("requires an owned transient test scope")
    sibling = os.path.join(os.path.dirname(__file__), "access_boundary_fixture.py")
    with contextlib.ExitStack() as stack:
        scratch = stack.enter_context(tempfile.TemporaryDirectory(prefix="subuid-broker-", dir="/tmp/opencode"))
        os.chmod(scratch, 0o733)
        directory = os.path.join(scratch, "broker")
        database = os.path.join(directory, "synthetic.db")

        def cleanup_owned_directory():
            subprocess.run(subuid_command(CLEANUP, directory), stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                           env={"PATH": "/usr/bin:/bin"}, timeout=5, check=True)

        stack.callback(cleanup_owned_directory)

        def start(code, path):
            parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            parent.settimeout(5)
            stack.callback(parent.close)
            stack.callback(child.close)
            process = subprocess.Popen(
                subuid_command(code, path, str(child.fileno())),
                pass_fds=(child.fileno(),), stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                env={"PATH": "/usr/bin:/bin"},
            )
            stack.callback(stop, process)
            child.close()
            return process, parent

        gate, gate_peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        gate.settimeout(5)
        stack.callback(gate.close)
        stack.callback(gate_peer.close)
        guard = subprocess.Popen(
            [sys.executable, sibling, "closed_gate", str(gate_peer.fileno())],
            pass_fds=(gate_peer.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, guard)
        gate_peer.close()
        broker, broker_sock = start(BROKER, directory)
        ready = broker_sock.recv(128).decode().split()
        if len(ready) != 2 or ready[0] != "READY" or int(ready[1]) != broker.pid:
            raise RuntimeError("broker did not become ready")
        owner_uid = effective_uid(broker.pid)
        if owner_uid == os.geteuid():
            raise RuntimeError("broker UID was not separated")
        try:
            fd = os.open(database, os.O_RDWR)
        except PermissionError:
            pass
        else:
            os.close(fd)
            raise RuntimeError("original UID unexpectedly accessed private database")
        print("original_uid_direct_access=denied", flush=True)
        broker_sock.sendall(b"B")
        if broker_sock.recv(1) != b"T":
            raise RuntimeError("broker transaction not started")
        broker.kill()
        broker.wait(timeout=5)
        broker, broker_sock = start(BROKER, directory)
        if not broker_sock.recv(128).startswith(b"READY "):
            raise RuntimeError("broker restart failed")
        if effective_uid(broker.pid) != owner_uid:
            raise RuntimeError("broker restart identity changed")
        broker_sock.sendall(b"R")
        if broker_sock.recv(128) != b"0 ok":
            raise RuntimeError("killed broker transaction was not rolled back")
        print("broker_death_rollback=verified", flush=True)
        outsider, outsider_sock = start(OUTSIDER, database)
        ready = outsider_sock.recv(128).decode().split()
        if len(ready) != 2 or ready[0] != "DIRECT" or int(ready[1]) != outsider.pid:
            raise RuntimeError("outsider did not start")
        if effective_uid(outsider.pid) != owner_uid:
            raise RuntimeError("outsider did not obtain broker host UID")
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate opened without exclusivity proof")
        outsider_sock.sendall(b"C")
        if outsider_sock.recv(128) != b"COMMITTED" or outsider.wait(timeout=5) != 0:
            raise RuntimeError("outsider did not commit")
        broker_sock.sendall(b"R")
        if broker_sock.recv(128) != b"1 ok":
            raise RuntimeError("outsider commit missing")
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate reopened without exclusivity proof")
        print("same_subuid_outsider_direct_commit=verified", flush=True)
        print("global_exclusivity_unproven=closed", flush=True)
        broker_sock.sendall(b"Q")
        if broker.wait(timeout=5) != 0:
            raise RuntimeError("broker did not stop")
        gate.close()
        if guard.wait(timeout=5) != 0:
            raise RuntimeError("guard did not stop")


if __name__ == "__main__":
    if len(sys.argv) != 1:
        raise SystemExit("invalid fixture arguments")
    run()

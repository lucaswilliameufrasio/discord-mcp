"""Opt-in synthetic SQLite access-boundary experiment; not application code."""
import contextlib
import os
import socket
import sqlite3
import subprocess
import sys
import tempfile


CLIENT = r'''
import os, socket, sqlite3, sys
path = sys.argv[1]
control = socket.socket(fileno=int(sys.argv[2]))
broker = socket.socket(fileno=int(sys.argv[3]))
control.settimeout(5)
broker.settimeout(5)
def direct_denied():
    try:
        fd = os.open(path, os.O_RDWR)
    except PermissionError:
        pass
    else:
        os.close(fd)
        return False
    try:
        connection = sqlite3.connect(f"file:{path}?mode=rw", uri=True, timeout=1)
    except sqlite3.OperationalError:
        return True
    connection.close()
    return False
if not direct_denied():
    raise RuntimeError("direct filesystem/SQLite access unexpectedly allowed")
control.sendall(f"DENIED {os.getpid()}".encode())
broker.sendall(b"B")
if broker.recv(1) != b"T":
    raise RuntimeError("broker did not start transaction")
control.sendall(b"INFLIGHT")
action = control.recv(1)
if action == b"C":
    broker.sendall(b"C")
    if broker.recv(1) != b"K":
        raise RuntimeError("broker did not commit")
    control.sendall(b"COMMITTED")
elif action == b"K":
    if broker.recv(1) != b"":
        raise RuntimeError("broker unexpectedly survived")
    if not direct_denied():
        raise RuntimeError("direct access allowed after broker death")
    control.sendall(b"DEAD_DENIED")
else:
    raise RuntimeError("unexpected test action")
'''


def broker(database, socket_fd):
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    connection = sqlite3.connect(database, timeout=5)
    if sock.recv(1) != b"B":
        raise RuntimeError("expected begin")
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    sock.sendall(b"T")
    if sock.recv(1) != b"C":
        raise RuntimeError("expected commit")
    connection.commit()
    connection.close()
    sock.sendall(b"K")
    sock.close()


def stop(process):
    if process.poll() is None:
        process.kill()
    process.communicate(timeout=5)


def run():
    with open("/proc/self/cgroup", encoding="ascii") as source:
        lines = source.read().splitlines()
    if len(lines) != 1 or not lines[0].startswith("0::/"):
        raise RuntimeError("expected cgroup v2")
    name = os.path.basename(lines[0][3:])
    if not name.startswith("discord-mcp-cgroup-drill-") or not name.endswith(".scope"):
        raise RuntimeError("requires an owned transient test scope")
    with contextlib.ExitStack() as stack:
        scratch = stack.enter_context(tempfile.TemporaryDirectory(prefix="access-boundary-", dir="/tmp/opencode"))
        if os.stat(scratch).st_mode & 0o077:
            raise RuntimeError("fixture directory is not private")
        database = os.path.join(scratch, "synthetic.db")
        with sqlite3.connect(database) as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
        for action in (b"C", b"K"):
            broker_end, client_end = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            controller, client_control = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            for sock in (broker_end, client_end, controller, client_control):
                stack.callback(sock.close)
            controller.settimeout(5)
            service = subprocess.Popen(
                [sys.executable, __file__, "broker", database, str(broker_end.fileno())],
                pass_fds=(broker_end.fileno(),), stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                env={"PATH": "/usr/bin:/bin"},
            )
            stack.callback(stop, service)
            writer = subprocess.Popen(
                ["/usr/bin/unshare", "--user", "--map-auto", "--map-root-user",
                 "--setgid", "1", "--setuid", "1", "/usr/bin/python3", "-c",
                 CLIENT, database, str(client_control.fileno()), str(client_end.fileno())],
                pass_fds=(client_control.fileno(), client_end.fileno()),
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                env={"PATH": "/usr/bin:/bin"},
            )
            stack.callback(stop, writer)
            broker_end.close()
            client_end.close()
            client_control.close()
            message = controller.recv(128).decode().split()
            if len(message) != 2 or message[0] != "DENIED" or int(message[1]) != writer.pid:
                raise RuntimeError("direct access was not denied")
            with open(f"/proc/{writer.pid}/status", encoding="ascii") as source:
                fields = dict(line.rstrip().split(":", 1) for line in source if ":" in line)
            if int(fields["Uid"].split()[1]) == os.geteuid() or int(fields["CapEff"].strip(), 16) != 0:
                raise RuntimeError("client identity/capabilities were not separated")
            if controller.recv(128) != b"INFLIGHT":
                raise RuntimeError("mediated transaction did not start")
            with sqlite3.connect(database) as db:
                expected = 0 if action == b"C" else 1
                if db.execute("SELECT COUNT(*) FROM writes").fetchone() != (expected,):
                    raise RuntimeError("uncommitted write was visible")
            if action == b"K":
                service.kill()
                service.wait(timeout=5)
            controller.sendall(action)
            expected_reply = b"COMMITTED" if action == b"C" else b"DEAD_DENIED"
            if controller.recv(128) != expected_reply:
                raise RuntimeError("unexpected client result")
            if writer.wait(timeout=5) != 0:
                raise RuntimeError("client failed")
            if action == b"C" and service.wait(timeout=5) != 0:
                raise RuntimeError("broker failed")
            with sqlite3.connect(database) as db:
                if db.execute("SELECT COUNT(*) FROM writes").fetchone() != (1,):
                    raise RuntimeError("unexpected persisted state")
                if db.execute("PRAGMA integrity_check").fetchone() != ("ok",):
                    raise RuntimeError("synthetic SQLite integrity failed")
            if action == b"C":
                print("direct_access_denied_mediated_commit_verified=yes", flush=True)
            else:
                print("broker_death_rolls_back_and_direct_access_denied=yes", flush=True)
                print("recovery_decision=closed", flush=True)


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "broker":
        broker(sys.argv[2], int(sys.argv[3]))
    elif len(sys.argv) == 1:
        run()
    else:
        raise SystemExit("invalid fixture arguments")

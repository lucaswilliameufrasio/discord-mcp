"""Opt-in Linux cgroup research inside an owned transient systemd scope only."""
import os
import select
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile


def populated(path):
    with open(os.path.join(path, "cgroup.events"), encoding="ascii") as events:
        fields = dict(line.split() for line in events)
    return fields["populated"] == "1"


IDENTITY_WORKER = r'''
import os, socket, sqlite3, sys
sock = socket.socket(fileno=int(sys.argv[2]))
sock.settimeout(5)
if os.fork():
    os._exit(0)
os.setsid()
connection = None
if sys.argv[3] != "-":
    database = os.path.join(sys.argv[3], "synthetic.db")
    connection = sqlite3.connect(database, timeout=5)
    connection.execute("PRAGMA journal_mode=WAL")
    connection.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
    connection.commit()
    snapshot = sqlite3.connect(os.path.join(sys.argv[3], "snapshot.db"), timeout=5)
    connection.backup(snapshot)
    snapshot.close()
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO writes VALUES (1)")
    reader = sqlite3.connect(database, timeout=5)
    if reader.execute("SELECT COUNT(*) FROM writes").fetchone()[0] != 0:
        raise RuntimeError("uncommitted write became visible")
    reader.close()
sock.sendall(f"READY {os.getpid()}".encode())
if sock.recv(1) != b"M":
    raise SystemExit(2)
try:
    os.setuid(0)
    regained = True
except PermissionError:
    regained = False
try:
    with open(os.path.join(sys.argv[1], "cgroup.procs"), "w", encoding="ascii") as target:
        target.write(str(os.getpid()))
except PermissionError:
    result = "DENIED"
else:
    result = "MOVED"
sock.sendall(f"{result} {int(regained)}".encode())
if connection is not None:
    if sock.recv(1) != b"C":
        raise SystemExit(4)
    connection.commit()
    reader = sqlite3.connect(database, timeout=5)
    count = reader.execute("SELECT COUNT(*) FROM writes").fetchone()[0]
    reader.close()
    fresh = sqlite3.connect(os.path.join(sys.argv[3], "fresh.db"), timeout=5)
    connection.backup(fresh)
    fresh.close()
    connection.close()
    sock.sendall(f"COMMITTED {count}".encode())
if sock.recv(1) != b"Q":
    raise SystemExit(3)
'''


def worker(leaf, scope, socket_fd, separate_identity=False, database_dir="-"):
    sock = socket.socket(fileno=socket_fd)
    sock.settimeout(5)
    with open(os.path.join(leaf, "cgroup.procs"), "w", encoding="ascii") as target:
        target.write(str(os.getpid()))
    pid = os.fork()
    if pid:
        os._exit(0)
    os.setsid()
    if separate_identity:
        os.set_inheritable(sock.fileno(), True)
        os.execv("/usr/bin/unshare", [
            "/usr/bin/unshare", "--user", "--map-auto", "--map-root-user",
            "--setgid", "1", "--setuid", "1", "/usr/bin/python3", "-c",
            IDENTITY_WORKER, scope, str(sock.fileno()), database_dir,
        ])
    sock.sendall(f"READY {os.getpid()}".encode())
    if sock.recv(1) != b"M":
        os._exit(2)
    try:
        # Attempt to move only this fixture process to its own transient scope,
        # never to a shared parent cgroup.
        with open(os.path.join(scope, "cgroup.procs"), "w", encoding="ascii") as target:
            target.write(str(os.getpid()))
    except PermissionError:
        sock.sendall(b"DENIED")
    else:
        sock.sendall(b"MOVED")
    if sock.recv(1) != b"Q":
        os._exit(3)
    sock.close()
    os._exit(0)


def controller(leaf, scope, socket_fd, database_dir="-"):
    process = subprocess.Popen(
        [sys.executable, __file__, "worker", leaf, scope, str(socket_fd), "identity", database_dir],
        pass_fds=(socket_fd,), stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        env={"PATH": "/usr/bin:/bin"},
    )
    os.close(socket_fd)
    if process.wait(timeout=5) != 0:
        raise RuntimeError("launcher failed")
    print("CONTROLLER_READY", flush=True)
    sys.stdin.readline()


def recovery(leaf):
    # Observation only: no policy here grants re-opening.
    print(f"populated={int(populated(leaf))} decision=closed", flush=True)


def continuity_recovery(database_dir, snapshot_name):
    """Compare only the controlled synthetic fixture, never authorize admission."""
    paths = [os.path.join(database_dir, "synthetic.db"), os.path.join(database_dir, snapshot_name)]
    try:
        for path in paths:
            if not os.path.isfile(path) or any(os.path.exists(path + suffix) for suffix in ("-wal", "-journal")):
                raise RuntimeError("missing file or sidecar requires reconciliation")
        results = []
        for path in paths:
            # Only after the fixture writer has exited and sidecars are absent.
            connection = sqlite3.connect(f"file:{path}?mode=ro&immutable=1", uri=True, timeout=5)
            try:
                integrity = connection.execute("PRAGMA integrity_check").fetchone()
                rows = connection.execute("SELECT value FROM writes ORDER BY value").fetchall()
            finally:
                connection.close()
            if integrity != ("ok",):
                raise RuntimeError("integrity check failed")
            results.append(rows)
        outcome = "matched" if results[0] == results[1] else "stale"
    except (OSError, RuntimeError, sqlite3.Error):
        outcome = "unavailable"
    print(f"continuity={outcome} decision=closed", flush=True)


def run(separate_identity=False, controller_crash=False, sqlite_transaction=False):
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise RuntimeError("Linux pidfd APIs required")
    with open("/proc/self/cgroup", encoding="ascii") as source:
        lines = source.read().splitlines()
    if len(lines) != 1 or not lines[0].startswith("0::/"):
        raise RuntimeError("expected a unified cgroup v2 hierarchy")
    relative = lines[0][3:]
    name = os.path.basename(relative)
    if not name.startswith("discord-mcp-cgroup-drill-") or not name.endswith(".scope"):
        raise RuntimeError("requires an owned discord-mcp-cgroup-drill transient scope")
    scope = os.path.join("/sys/fs/cgroup", relative.lstrip("/"))
    scratch = None
    if sqlite_transaction:
        scratch = tempfile.TemporaryDirectory(prefix="cgroup-sqlite-", dir="/tmp/opencode")
        # Only the owning test process can list the directory; the subordinate
        # identity may create its known synthetic SQLite file inside it.
        os.chmod(scratch.name, 0o733)
    leaf = os.path.join(scope, f"fixture-{os.getpid()}")
    os.mkdir(leaf)
    parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    parent.settimeout(5)
    process = None
    pidfd = None
    try:
        role = "controller" if controller_crash else "worker"
        args = [sys.executable, __file__, role, leaf, scope, str(child.fileno())]
        if separate_identity and not controller_crash:
            args.append("identity")
        if sqlite_transaction:
            args.append(scratch.name)
        process = subprocess.Popen(
            args, pass_fds=(child.fileno(),),
            stdin=subprocess.PIPE if controller_crash else subprocess.DEVNULL,
            stdout=subprocess.PIPE if controller_crash else subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env={"PATH": "/usr/bin:/bin"},
        )
        child.close()
        message = parent.recv(128).decode().split()
        if len(message) != 2 or message[0] != "READY":
            raise RuntimeError("descendant did not become ready")
        pidfd = os.pidfd_open(int(message[1]))
        if separate_identity:
            with open(f"/proc/{int(message[1])}/status", encoding="ascii") as status:
                fields = dict(line.rstrip().split(":", 1) for line in status if ":" in line)
            effective_uid = int(fields["Uid"].split()[1])
            if effective_uid == os.geteuid() or int(fields["CapEff"].strip(), 16) != 0:
                raise RuntimeError("worker identity/capabilities were not separated")
            print("worker_identity_separated_and_no_effective_caps=yes", flush=True)
        if controller_crash:
            if not select.select([process.stdout], [], [], 5)[0]:
                raise TimeoutError("controller did not become ready")
            if process.stdout.readline().strip() != b"CONTROLLER_READY":
                raise RuntimeError("controller failed before ready")
            process.kill()
            process.wait(timeout=5)
            print("controller_killed_while_descendant_alive=yes", flush=True)
            observed = subprocess.run(
                [sys.executable, __file__, "recovery", leaf],
                capture_output=True, text=True, timeout=5, check=True,
                env={"PATH": "/usr/bin:/bin"},
            )
            if observed.stdout.strip() != "populated=1 decision=closed":
                raise RuntimeError("recovery did not observe a populated closed group")
            print("recovery_after_controller_death=populated_closed", flush=True)
        elif process.wait(timeout=5) != 0:
            raise RuntimeError("launcher failed")
        if not populated(leaf):
            raise RuntimeError("descendant not observed in leaf after launcher exit")
        print("descendant_inherited_leaf_after_parent_exit=yes", flush=True)
        parent.sendall(b"M")
        result = parent.recv(128)
        if separate_identity:
            parts = result.split()
            if len(parts) != 2 or parts[1] != b"0":
                raise RuntimeError("worker could regain namespace uid 0 or response invalid")
            result = parts[0]
        if result == b"MOVED":
            if populated(leaf):
                raise RuntimeError("moved descendant still appears in leaf")
            print("self_migration_to_scope=allowed", flush=True)
            print("empty_leaf_while_descendant_alive=yes", flush=True)
        elif result == b"DENIED":
            if not populated(leaf):
                raise RuntimeError("denied descendant disappeared from leaf")
            print("self_migration_to_scope=denied", flush=True)
        else:
            raise RuntimeError("invalid migration response")
        print("admission_decision=closed_unqualified_boundary", flush=True)
        if sqlite_transaction:
            parent.sendall(b"C")
            if parent.recv(128) != b"COMMITTED 1":
                raise RuntimeError("synthetic transaction was not committed")
            if not populated(leaf):
                raise RuntimeError("writer disappeared after commit")
            observed = subprocess.run(
                [sys.executable, __file__, "recovery", leaf],
                capture_output=True, text=True, timeout=5, check=True,
                env={"PATH": "/usr/bin:/bin"},
            )
            if observed.stdout.strip() != "populated=1 decision=closed":
                raise RuntimeError("recovery did not stay closed after commit")
            print("sqlite_commit_after_controller_death=verified", flush=True)
            print("recovery_after_sqlite_commit=populated_closed", flush=True)
        parent.sendall(b"Q")
        if not select.select([pidfd], [], [], 5)[0]:
            raise TimeoutError("descendant did not exit")
        if populated(leaf):
            raise RuntimeError("leaf remained populated after descendant exit")
        print("owned_leaf_empty_after_exit=yes", flush=True)
        if controller_crash:
            observed = subprocess.run(
                [sys.executable, __file__, "recovery", leaf],
                capture_output=True, text=True, timeout=5, check=True,
                env={"PATH": "/usr/bin:/bin"},
            )
            if observed.stdout.strip() != "populated=0 decision=closed":
                raise RuntimeError("recovery did not remain closed after exit")
            print("recovery_after_descendant_exit=empty_closed", flush=True)
        if sqlite_transaction:
            def compare(snapshot_name):
                result = subprocess.run(
                    [sys.executable, __file__, "continuity", scratch.name, snapshot_name],
                    capture_output=True, text=True, timeout=5, check=True,
                    env={"PATH": "/usr/bin:/bin"},
                )
                return result.stdout.strip()

            if compare("snapshot.db") != "continuity=stale decision=closed":
                raise RuntimeError("stale snapshot was not rejected")
            print("pre_crash_snapshot=stale_closed", flush=True)
            fresh_result = compare("fresh.db")
            if fresh_result != "continuity=matched decision=closed":
                raise RuntimeError(f"matching snapshot check: {fresh_result}")
            print("post_commit_snapshot=matched_closed", flush=True)
            if compare("missing.db") != "continuity=unavailable decision=closed":
                raise RuntimeError("missing snapshot was not rejected")
            print("missing_snapshot=unavailable_closed", flush=True)
    finally:
        parent.close()
        child.close()
        if process is not None:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=5)
        if pidfd is not None:
            if not select.select([pidfd], [], [], 0)[0]:
                signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                select.select([pidfd], [], [], 5)
            os.close(pidfd)
        os.rmdir(leaf)
        if scratch is not None:
            scratch.cleanup()


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "controller":
        controller(sys.argv[2], sys.argv[3], int(sys.argv[4]))
    elif len(sys.argv) == 6 and sys.argv[1] == "controller":
        controller(sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5])
    elif len(sys.argv) == 3 and sys.argv[1] == "recovery":
        recovery(sys.argv[2])
    elif len(sys.argv) == 4 and sys.argv[1] == "continuity":
        continuity_recovery(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 5 and sys.argv[1] == "worker":
        worker(sys.argv[2], sys.argv[3], int(sys.argv[4]))
    elif len(sys.argv) in (6, 7) and sys.argv[1] == "worker" and sys.argv[5] == "identity":
        worker(sys.argv[2], sys.argv[3], int(sys.argv[4]), separate_identity=True,
               database_dir=sys.argv[6] if len(sys.argv) == 7 else "-")
    elif len(sys.argv) == 1:
        run()
    elif len(sys.argv) == 2 and sys.argv[1] == "--identity":
        run(separate_identity=True)
    elif len(sys.argv) == 2 and sys.argv[1] == "--controller-crash":
        run(separate_identity=True, controller_crash=True)
    elif len(sys.argv) == 2 and sys.argv[1] == "--sqlite-controller-crash":
        run(separate_identity=True, controller_crash=True, sqlite_transaction=True)
    else:
        raise SystemExit("invalid fixture arguments")

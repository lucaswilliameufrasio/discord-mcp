"""Opt-in Linux cgroup research inside an owned transient systemd scope only."""
import os
import signal
import socket
import subprocess
import sys


def populated(path):
    with open(os.path.join(path, "cgroup.events"), encoding="ascii") as events:
        fields = dict(line.split() for line in events)
    return fields["populated"] == "1"


IDENTITY_WORKER = r'''
import os, socket, sys
sock = socket.socket(fileno=int(sys.argv[2]))
sock.settimeout(5)
if os.fork():
    os._exit(0)
os.setsid()
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
if sock.recv(1) != b"Q":
    raise SystemExit(3)
'''


def worker(leaf, scope, socket_fd, separate_identity=False):
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
            IDENTITY_WORKER, scope, str(sock.fileno()),
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


def run(separate_identity=False):
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
    leaf = os.path.join(scope, f"fixture-{os.getpid()}")
    os.mkdir(leaf)
    parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    parent.settimeout(5)
    process = None
    pidfd = None
    try:
        args = [sys.executable, __file__, "worker", leaf, scope, str(child.fileno())]
        if separate_identity:
            args.append("identity")
        process = subprocess.Popen(
            args,
            pass_fds=(child.fileno(),), stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
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
        if process.wait(timeout=5) != 0:
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
        parent.sendall(b"Q")
        import select
        if not select.select([pidfd], [], [], 5)[0]:
            raise TimeoutError("descendant did not exit")
        if populated(leaf):
            raise RuntimeError("leaf remained populated after descendant exit")
        print("owned_leaf_empty_after_exit=yes", flush=True)
    finally:
        parent.close()
        child.close()
        if process is not None:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=5)
        if pidfd is not None:
            import select
            if not select.select([pidfd], [], [], 0)[0]:
                signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                select.select([pidfd], [], [], 5)
            os.close(pidfd)
        os.rmdir(leaf)


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "worker":
        worker(sys.argv[2], sys.argv[3], int(sys.argv[4]))
    elif len(sys.argv) == 6 and sys.argv[1] == "worker" and sys.argv[5] == "identity":
        worker(sys.argv[2], sys.argv[3], int(sys.argv[4]), separate_identity=True)
    elif len(sys.argv) == 1:
        run()
    elif len(sys.argv) == 2 and sys.argv[1] == "--identity":
        run(separate_identity=True)
    else:
        raise SystemExit("invalid fixture arguments")

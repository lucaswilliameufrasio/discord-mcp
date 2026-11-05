"""Opt-in namespace/descriptor experiment; never an application gate."""
import contextlib
import os
import socket
import sqlite3
import subprocess
import sys
import tempfile


CHILD = r'''
import ctypes, errno, os, socket, sys
directory, database = sys.argv[1:3]
control_fd = int(sys.argv[3])
owner_pid = int(sys.argv[4])
sock = socket.socket(fileno=control_fd)
sock.settimeout(5)
# A closed allowlist, not a scan for only one pathname: reject any unknown fd.
for name in os.listdir("/proc/self/fd"):
    fd = int(name)
    try:
        os.fstat(fd)
    except OSError:
        continue
    if fd not in (0, 1, 2, control_fd):
        sock.sendall(b"REJECT_FD")
        raise SystemExit(0)
libc = ctypes.CDLL(None, use_errno=True)
libc.mount.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_ulong, ctypes.c_char_p]
libc.umount2.argtypes = [ctypes.c_char_p, ctypes.c_int]
target = os.fsencode(directory)
if libc.mount(b"tmpfs", target, b"tmpfs", 14, b"size=1048576,mode=0700") != 0:
    raise OSError(ctypes.get_errno(), "private tmpfs mount failed")
os.setgroups([])
os.setresgid(1, 1, 1)
os.setresuid(1, 1, 1)
if libc.prctl(38, 1, 0, 0, 0) != 0:
    raise OSError(ctypes.get_errno(), "no_new_privs failed")
try:
    os.setuid(0)
except PermissionError:
    pass
else:
    raise RuntimeError("namespace UID 0 was regained")
if libc.umount2(target, 0) == 0 or ctypes.get_errno() != errno.EPERM:
    raise RuntimeError("unmount was not denied with EPERM")
for path in (database, f"/proc/{owner_pid}/root{database}"):
    try:
        fd = os.open(path, os.O_RDWR)
    except (PermissionError, FileNotFoundError):
        continue
    os.close(fd)
    raise RuntimeError("synthetic database was accessible")
sock.sendall(f"RESTRICTED {os.getpid()}".encode())
if sock.recv(1) != b"Q":
    raise RuntimeError("expected exit")
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
        scratch = stack.enter_context(tempfile.TemporaryDirectory(prefix="hardened-mount-", dir="/tmp/opencode"))
        database = os.path.join(scratch, "synthetic.db")
        with sqlite3.connect(database) as db:
            db.execute("CREATE TABLE writes(value INTEGER NOT NULL)")
        gate, gate_peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        for sock in (gate, gate_peer):
            stack.callback(sock.close)
        gate.settimeout(5)
        guard = subprocess.Popen(
            [sys.executable, sibling, "closed_gate", str(gate_peer.fileno())],
            pass_fds=(gate_peer.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, guard)
        gate_peer.close()

        def spawn(leak):
            parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            parent.settimeout(5)
            stack.callback(parent.close)
            stack.callback(child.close)
            inherited = [child.fileno()]
            leaked_fd = None
            if leak:
                leaked_fd = os.open(database, os.O_RDWR)
                inherited.append(leaked_fd)
            try:
                process = subprocess.Popen(
                    ["/usr/bin/unshare", "--user", "--map-auto", "--map-root-user", "--mount",
                     "--propagation", "private", "/usr/bin/python3", "-c", CHILD,
                     scratch, database, str(child.fileno()), str(os.getpid())],
                    pass_fds=tuple(inherited), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
                )
            finally:
                if leaked_fd is not None:
                    os.close(leaked_fd)
            stack.callback(stop, process)
            child.close()
            return parent, process

        leaky_socket, leaky = spawn(True)
        if leaky_socket.recv(128) != b"REJECT_FD" or leaky.wait(timeout=5) != 0:
            raise RuntimeError("inherited descriptor was not rejected")
        print("inherited_database_descriptor=launch_rejected", flush=True)
        restricted_socket, restricted = spawn(False)
        message = restricted_socket.recv(128).decode().split()
        if len(message) != 2 or message[0] != "RESTRICTED" or int(message[1]) != restricted.pid:
            raise RuntimeError("restricted process did not complete probes")
        with open(f"/proc/{restricted.pid}/status", encoding="ascii") as source:
            fields = dict(line.rstrip().split(":", 1) for line in source if ":" in line)
        if int(fields["Uid"].split()[1]) == os.geteuid() or int(fields["CapEff"].strip(), 16) != 0:
            raise RuntimeError("namespace identity/capabilities not restricted")
        print("restricted_uid_unmount_and_paths=denied", flush=True)
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate opened without exclusivity proof")

        # Deliberate counterexample: a process not subject to the namespace
        # policy retains the broker UID and can still write the private file.
        direct, direct_peer = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        direct.settimeout(5)
        stack.callback(direct.close)
        stack.callback(direct_peer.close)
        outside = subprocess.Popen(
            [sys.executable, sibling, "same_uid_writer", database, str(direct_peer.fileno())],
            pass_fds=(direct_peer.fileno(),), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, env={"PATH": "/usr/bin:/bin"},
        )
        stack.callback(stop, outside)
        direct_peer.close()
        if not direct.recv(128).startswith(b"DIRECT "):
            raise RuntimeError("outside writer did not start")
        direct.sendall(b"C")
        if direct.recv(128) != b"COMMITTED" or outside.wait(timeout=5) != 0:
            raise RuntimeError("outside writer did not commit")
        with sqlite3.connect(database) as db:
            if db.execute("SELECT COUNT(*) FROM writes").fetchone() != (1,):
                raise RuntimeError("outside synthetic commit missing")
        for request in (b"W", b"C"):
            gate.sendall(request)
            if gate.recv(1) != b"N":
                raise RuntimeError("gate reopened without exclusivity proof")
        print("unconfined_same_uid_writer=committed", flush=True)
        print("global_exclusivity_unproven=closed", flush=True)
        restricted_socket.sendall(b"Q")
        if restricted.wait(timeout=5) != 0:
            raise RuntimeError("restricted process failed")
        gate.close()
        if guard.wait(timeout=5) != 0:
            raise RuntimeError("guard did not exit")


if __name__ == "__main__":
    if len(sys.argv) != 1:
        raise SystemExit("invalid fixture arguments")
    run()

"""Linux-only descriptor-anchor experiment. Not an operational gate."""
import fcntl
import os
import select
import subprocess
import sys
import tempfile
import unittest


def child(role, fd):
    if role == "writer":
        fcntl.flock(fd, fcntl.LOCK_SH)
        print("HOLDING", flush=True)
        if sys.stdin.readline().strip() != "release":
            raise RuntimeError("expected release")
    elif role == "keeper":
        print("ANCHORED", flush=True)
        sys.stdin.readline()
    elif role == "guard":
        print("OPEN" if os.read(fd, 64) == b"clean\n" else "CLOSED", flush=True)
    else:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("BUSY", flush=True)
        else:
            print("ACQUIRED", flush=True)
    os.close(fd)


def line(stream):
    ready, _, _ = select.select([stream], [], [], 5)
    if not ready:
        raise TimeoutError("fixture child timed out")
    result = stream.readline().strip()
    if not result:
        raise RuntimeError("fixture child exited without result")
    return result


class AnchorTests(unittest.TestCase):
    def test_anchor_death_keeps_restart_closed_with_old_writer(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            marker = os.path.join(directory, "state")
            with open(path, "wb"):
                pass
            with open(marker, "wb") as state:
                state.write(b"clean\n")
                state.flush()
                os.fsync(state.fileno())
            children = []

            def cleanup():
                for process in children:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

            self.addCleanup(cleanup)

            def spawn(role, fd):
                process = subprocess.Popen(
                    [sys.executable, __file__, role, str(fd)],
                    pass_fds=(fd,), stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                children.append(process)
                return process

            def guarded_restart():
                fd = os.open(marker, os.O_RDONLY)
                try:
                    process = spawn("guard", fd)
                finally:
                    os.close(fd)
                result = line(process.stdout)
                self.assertEqual(process.wait(timeout=5), 0)
                return result

            self.assertEqual(guarded_restart(), "OPEN")
            # The fixture assumes this state is durably written before any
            # descriptor is distributed. There is no clean transition here.
            with open(marker, "wb") as state:
                state.write(b"uncertain\n")
                state.flush()
                os.fsync(state.fileno())
            anchor = os.open(path, os.O_RDWR)
            keeper = spawn("keeper", anchor)
            self.assertEqual(line(keeper.stdout), "ANCHORED")
            writer_fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
            writer = spawn("writer", writer_fd)
            os.close(writer_fd)
            self.assertEqual(line(writer.stdout), "HOLDING")
            os.close(anchor)
            replacement = os.path.join(directory, "replacement")
            with open(replacement, "wb"):
                pass
            os.replace(replacement, path)
            keeper.kill()
            keeper.wait(timeout=5)
            self.assertIsNone(writer.poll())
            # Locking the replacement is possible but must not clear the state.
            replacement_fd = os.open(path, os.O_RDWR)
            try:
                fcntl.flock(replacement_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertEqual(guarded_restart(), "CLOSED")
            finally:
                os.close(replacement_fd)
            writer.stdin.write("release\n")
            writer.stdin.flush()
            self.assertEqual(writer.wait(timeout=5), 0)
            self.assertEqual(guarded_restart(), "CLOSED")
            with open(marker, "rb") as state:
                self.assertEqual(state.read(), b"uncertain\n")

    def test_anchor_keeps_all_participants_on_original_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "activity.lock")
            with open(path, "wb"):
                pass
            anchor = os.open(path, os.O_RDWR)
            self.addCleanup(os.close, anchor)
            children = []

            def cleanup():
                for process in children:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

            self.addCleanup(cleanup)

            def start(role):
                # Reopen the pinned inode, not its replaceable pathname. A
                # separate open-file description is essential for flock.
                fd = os.open(f"/proc/self/fd/{anchor}", os.O_RDWR)
                self.assertEqual(os.fstat(fd).st_ino, os.fstat(anchor).st_ino)
                try:
                    process = subprocess.Popen(
                        [sys.executable, __file__, role, str(fd)],
                        pass_fds=(fd,), stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                finally:
                    os.close(fd)
                children.append(process)
                return process

            first = start("writer")
            self.assertEqual(line(first.stdout), "HOLDING")
            replacement = os.path.join(directory, "replacement")
            with open(replacement, "wb"):
                pass
            os.replace(replacement, path)
            self.assertNotEqual(os.stat(path).st_ino, os.fstat(anchor).st_ino)

            # A new writer and two independently started authorities continue
            # using the anchored inode despite the pathname replacement.
            second = start("writer")
            self.assertEqual(line(second.stdout), "HOLDING")
            for _ in range(2):
                authority = start("authority")
                self.assertEqual(line(authority.stdout), "BUSY")
                self.assertEqual(authority.wait(timeout=5), 0)
            first.stdin.write("release\n")
            first.stdin.flush()
            self.assertEqual(first.wait(timeout=5), 0)
            authority = start("authority")
            self.assertEqual(line(authority.stdout), "BUSY")
            self.assertEqual(authority.wait(timeout=5), 0)
            second.stdin.write("release\n")
            second.stdin.flush()
            self.assertEqual(second.wait(timeout=5), 0)
            authority = start("authority")
            self.assertEqual(line(authority.stdout), "ACQUIRED")
            self.assertEqual(authority.wait(timeout=5), 0)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] in ("writer", "authority", "keeper", "guard"):
        child(sys.argv[1], int(sys.argv[2]))
    else:
        unittest.main(verbosity=2)

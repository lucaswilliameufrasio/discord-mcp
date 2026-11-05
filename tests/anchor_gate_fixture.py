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
    if len(sys.argv) == 3 and sys.argv[1] in ("writer", "authority"):
        child(sys.argv[1], int(sys.argv[2]))
    else:
        unittest.main(verbosity=2)

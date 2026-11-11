"""Real terminal regression: a pager has piped stdin, unlike a normal TUI."""
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import unittest

BINARY = Path(__file__).resolve().parents[1] / 'target/debug/sql-viewer'
LONG_VALUE = 'beta-' + 'x' * 80
RULE = '+----+' + '-' * (len(LONG_VALUE) + 2) + '+'
DATA = ('\n'.join([RULE, '| id | ' + 'value'.ljust(len(LONG_VALUE)) + ' |', RULE,
                   '| 10 | ' + 'alpha'.ljust(len(LONG_VALUE)) + ' |',
                   '| 2  | ' + LONG_VALUE + ' |', RULE]) + '\n').encode()



@unittest.skipUnless(BINARY.exists(), 'cargo build --bin sql-viewer first')
class TerminalTest(unittest.TestCase):
    def test_zero_rows_never_enter_alternate_screen(self):
        # Wider than a normal screen: the usual size shortcut cannot hide a bug.
        border = '+' + '-' * 160 + '+'
        header = '|' + 'long_column_name'.ljust(160) + '|'
        table = '\n'.join([border, header, border, border]) + '\n'
        for data in [table, '\x1b[32m' + table + '\x1b[0mSELECT 0\n', '', '\n']:
            with self.subTest(data=data[:30]):
                master, slave = pty.openpty()
                try:
                    result = subprocess.run([str(BINARY), '--escaped'], input=data.encode(),
                                            stdout=slave, stderr=subprocess.PIPE, timeout=5)
                    output = bytearray()
                    while select.select([master], [], [], .1)[0]:
                        try:
                            chunk = os.read(master, 65536)
                        except OSError:
                            break
                        if not chunk:
                            break
                        output.extend(chunk)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertNotIn(b'\x1b[?1049h', output)
                    self.assertEqual(bytes(output).replace(b'\r\n', b'\n'), data.encode())
                finally:
                    if slave is not None:
                        os.close(slave)
                    os.close(master)

    def test_piped_input_navigation_copy_search_and_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / 'sql-viewer/theme'
            config.parent.mkdir()
            config.write_text('tokyo-night\n')
            copied = Path(directory) / 'copied'
            copier = Path(directory) / 'pbcopy'
            copier.write_text('#!/bin/sh\ncat > "$SQL_VIEWER_TEST_COPY"\n')
            copier.chmod(0o700)
            read_fd, write_fd = os.pipe()
            os.write(write_fd, DATA)
            os.close(write_fd)
            pid, master = pty.fork()
            if pid == 0:
                os.dup2(read_fd, 0)
                os.close(read_fd)
                os.environ.update(TERM='xterm-256color', COLORTERM='truecolor',
                                  PATH=directory + os.pathsep + os.environ['PATH'],
                                  SQL_VIEWER_TEST_COPY=str(copied), XDG_CONFIG_HOME=directory)
                os.execv(str(BINARY), [str(BINARY), '--escaped'])
            os.close(read_fd)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
            initial = termios.tcgetattr(master)
            output = bytearray()

            def wait_for(needle):
                deadline = time.monotonic() + 5
                while needle not in output and time.monotonic() < deadline:
                    if select.select([master], [], [], .1)[0]:
                        output.extend(os.read(master, 65536))
                self.assertIn(needle, output)
                output.clear()

            def wait_for_copy(expected):
                # Ratatui emits only changed cells, so a status message need not
                # appear as contiguous bytes. Check the clipboard result directly.
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if copied.exists() and copied.read_text() == expected:
                        break
                    if select.select([master], [], [], .1)[0]:
                        output.extend(os.read(master, 65536))
                self.assertEqual(copied.read_text() if copied.exists() else None, expected)
                output.clear()

            try:
                wait_for(b'tokyo-night')
                for name in ['terminal', 'catppuccin-mocha', 'catppuccin-latte', 'tokyo-night']:
                    os.write(master, b'T')
                    wait_for(f'Theme: {name} (saved)'.encode())
                    self.assertEqual(config.read_text().strip(), name)
                # Toggle twice, then verify navigation/copy still select the full cell.
                # Exact panel geometry is asserted by the TestBackend tests.
                os.write(master, b'ddGlv')
                wait_for_copy(LONG_VALUE)
                os.write(master, b'y')
                wait_for_copy('2\t' + LONG_VALUE)
                os.write(master, b'/alpha\r')
                # Search jumps to the matching cell; copying proves the selection.
                os.write(master, b'v')
                wait_for_copy('alpha')
                os.write(master, b'q')
                deadline = time.monotonic() + 5
                status = None
                while time.monotonic() < deadline:
                    ended, value = os.waitpid(pid, os.WNOHANG)
                    if ended:
                        status = value
                        pid = None
                        break
                    if select.select([master], [], [], .1)[0]:
                        try:
                            output.extend(os.read(master, 65536))
                        except OSError:
                            pass
                self.assertEqual(status, 0)
                restored = termios.tcgetattr(master)
                mask = termios.ICANON | termios.ECHO
                self.assertEqual(restored[3] & mask, initial[3] & mask)
                self.assertIn(b'\x1b[?1049l', output)
            finally:
                if pid:
                    os.kill(pid, signal.SIGKILL)
                    os.waitpid(pid, 0)
                os.close(master)


if __name__ == '__main__':
    unittest.main()

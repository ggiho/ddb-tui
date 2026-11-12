import importlib.machinery
import importlib.util
import pathlib
import subprocess
import unittest
from unittest.mock import patch
from types import SimpleNamespace
import io
import os
import tempfile

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "scripts/sql-pager"
loader = importlib.machinery.SourceFileLoader("pager", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
pager = importlib.util.module_from_spec(spec)
loader.exec_module(pager)


class PagerTests(unittest.TestCase):
    def test_display_width(self):
        self.assertEqual(pager.width("\x1b[32m서울e\u0301\x1b[0m"), 5)
        self.assertEqual(pager.width("a\tb"), 9)

    def test_both_dimensions_control_inline_output(self):
        self.assertTrue(pager.fits(b"abc\n", 3, 2))
        self.assertFalse(pager.fits(b"abcd\n", 3, 2))
        self.assertFalse(pager.fits(b"a\nb\n", 3, 2))

    def test_fit_check_stops_after_the_screen_edge(self):
        # A wide result needs the viewer as soon as it exceeds the viewport.
        # Do not walk a megabyte-long field merely to decide that it is wide.
        original = pager.unicodedata.east_asian_width
        calls = 0
        def bounded_width(char):
            nonlocal calls
            calls += 1
            self.assertLessEqual(calls, 81, 'scanned beyond the viewport')
            return original(char)
        with patch.object(pager.unicodedata, 'east_asian_width', side_effect=bounded_width):
            self.assertFalse(pager.fits(b'x' * 1_000_000, 80, 24))
        self.assertTrue(pager.fits('서울e\u0301'.encode(), 5, 24))
        self.assertFalse(pager.fits('서울e\u0301'.encode(), 4, 24))

    def test_redirect_preserves_exact_bytes(self):
        data = b"\x1b[31mraw\xff\n\n"
        result = subprocess.run(["python3", str(SCRIPT)], input=data, capture_output=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, data)
        self.assertEqual(result.stderr, b"")

    def test_unsupported_input_reaches_fallback_unchanged(self):
        data = b'not a supported table\n' * 100
        with patch.object(pager.sys, 'stdin', SimpleNamespace(buffer=io.BytesIO(data))), \
             patch.object(pager.os, 'get_terminal_size', return_value=(80, 24)), \
             patch.object(pager, 'run', side_effect=[2, 0]) as run:
            self.assertEqual(pager.main(), 0)
            self.assertEqual(run.call_count, 2)
            self.assertEqual(run.call_args_list[0].args[1], data)
            self.assertEqual(run.call_args_list[1].args[1], data)

    def test_missing_binary_reaches_fallback(self):
        data = b'x' * 100
        with patch.object(pager.sys, 'stdin', SimpleNamespace(buffer=io.BytesIO(data))), \
             patch.object(pager.sys, 'stderr', io.StringIO()), \
             patch.object(pager.os, 'get_terminal_size', return_value=(80, 24)), \
             patch.object(pager, 'run', side_effect=[FileNotFoundError(), 0]) as run:
            self.assertEqual(pager.main(), 0)
            self.assertEqual(run.call_args.args[1], data)

    def test_session_cache_replaces_last_result_and_resets_state(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = pathlib.Path(folder)
            with patch.dict(os.environ, SQL_VIEWER_SESSION=folder):
                self.assertEqual(pager.session_directory(), directory)
            pager.remember(directory, b'first result')
            (directory / 'view.json').write_text('{"row":3}')
            pager.remember(directory, b'next result')
            self.assertEqual((directory / 'result').read_bytes(), b'next result')
            self.assertFalse((directory / 'view.json').exists())
            with patch.object(pager, 'MAX_CACHE_BYTES', 1):
                pager.remember(directory, b'too large')
            self.assertFalse((directory / 'result').exists())

    def test_resume_uses_saved_bytes_and_bypasses_size_check(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = pathlib.Path(folder)
            pager.remember(directory, b'saved result')
            with patch.dict(os.environ, SQL_VIEWER_SESSION=folder), \
                 patch.object(pager.sys, 'argv', ['viewer', '--resume']), \
                 patch.object(pager.sys, 'stdin', SimpleNamespace(buffer=None)), \
                 patch.object(pager.os, 'get_terminal_size', return_value=(100, 100)), \
                 patch.object(pager, 'run', return_value=0) as run:
                self.assertEqual(pager.main(), 0)
                self.assertEqual(run.call_args.args[1], b'saved result')
                self.assertIn('--view-state', run.call_args.args[0])

    def test_sessions_do_not_leak_and_public_directory_is_rejected(self):
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            pager.remember(pathlib.Path(first), b'first connection')
            self.assertFalse((pathlib.Path(second) / 'result').exists())
            pathlib.Path(second).chmod(0o755)
            with patch.dict(os.environ, SQL_VIEWER_SESSION=second):
                self.assertIsNone(pager.session_directory())


if __name__ == "__main__":
    unittest.main()

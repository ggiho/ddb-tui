import importlib.machinery
import importlib.util
import pathlib
import subprocess
import unittest
from unittest.mock import patch
from types import SimpleNamespace
import io

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


if __name__ == "__main__":
    unittest.main()

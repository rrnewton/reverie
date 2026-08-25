#!/usr/bin/env python3
"""Tests for pair_verify_runs.

These exist because the module was WRONG TWICE in ways that produced plausible
numbers rather than errors, and neither failure would have been caught by running
it and reading the output:

  1. Pairing by modification time cross-paired different guests and silently
     dropped 41% of the population.
  2. The classifier matched only `finish syscall #N:` records, so `[iobuf]`
     buffer-content hashes fell through and every OBSERVATION divergence was
     misfiled as CONTROL-FLOW. That moved 128 of 711 pairs once fixed.

Each test below pins one of those, plus the three-class contract.
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pair_verify_runs as p  # noqa: E402

SYS = '[syscall][detcore, dtid 3] finish syscall #{n}: {call} = {ret}'
IOB = '[iobuf][dtid {d}] {op} {dir} {addr}+{ln}->{h}'


class DiffKind(unittest.TestCase):
    def test_time_payload_difference_is_clock(self):
        a = SYS.format(n=1, call='clock_gettime(CLOCK_MONOTONIC, 0x7f -> '
                                 '{ tv_sec: 10, tv_nsec: 5 })', ret='Ok(0)')
        b = a.replace('tv_nsec: 5', 'tv_nsec: 99')
        self.assertEqual(p.diff_kind(a, b), 'clock')

    def test_iobuf_hash_difference_is_observation_not_controlflow(self):
        """The regression that cost 128 pairs: same buffer, different bytes."""
        a = IOB.format(d=31, op='read', dir='in', addr='0xabc', ln=246, h='aa' * 32)
        b = IOB.format(d=31, op='read', dir='in', addr='0xabc', ln=246, h='bb' * 32)
        self.assertEqual(p.diff_kind(a, b), 'observation')

    def test_iobuf_with_different_length_is_controlflow(self):
        a = IOB.format(d=31, op='read', dir='in', addr='0xabc', ln=246, h='aa' * 32)
        b = IOB.format(d=31, op='read', dir='in', addr='0xabc', ln=99, h='aa' * 32)
        self.assertEqual(p.diff_kind(a, b), 'controlflow')

    def test_iobuf_on_a_different_thread_is_controlflow(self):
        a = IOB.format(d=31, op='read', dir='in', addr='0xabc', ln=246, h='aa' * 32)
        b = IOB.format(d=37, op='read', dir='in', addr='0xabc', ln=246, h='aa' * 32)
        self.assertEqual(p.diff_kind(a, b), 'controlflow')

    def test_different_return_value_is_controlflow(self):
        a = SYS.format(n=9, call='wait4(-1, 0x0, WNOHANG, NULL)', ret='Ok(7)')
        b = SYS.format(n=9, call='wait4(-1, 0x0, WNOHANG, NULL)', ret='Ok(0)')
        self.assertEqual(p.diff_kind(a, b), 'controlflow')

    def test_same_syscall_differing_payload_is_observation(self):
        a = SYS.format(n=9, call='uname(0x1 -> { node: "a" })', ret='Ok(0)')
        b = SYS.format(n=9, call='uname(0x1 -> { node: "b" })', ret='Ok(0)')
        self.assertEqual(p.diff_kind(a, b), 'observation')


class Shape(unittest.TestCase):
    def test_iobuf_shape_excludes_the_hash(self):
        a = IOB.format(d=3, op='recvmsg', dir='in', addr='0xf00', ln=1468, h='11' * 32)
        b = IOB.format(d=3, op='recvmsg', dir='in', addr='0xf00', ln=1468, h='22' * 32)
        self.assertEqual(p.shape(a), p.shape(b))
        self.assertEqual(p.shape(a)[0], 'iobuf')

    def test_unrecognised_line_does_not_masquerade_as_a_match(self):
        """(None, None, None) must not compare equal-and-meaningful."""
        self.assertEqual(p.shape('some unstructured line'), (None, None, None))
        self.assertEqual(p.diff_kind('unstructured A', 'unstructured B'), 'controlflow')


class Loading(unittest.TestCase):
    def test_load_extracts_detlog_and_first_execve(self):
        with tempfile.NamedTemporaryFile('w', suffix='.log', delete=False) as fh:
            fh.write('noise\n')
            fh.write('t INFO x: DETLOG first line\n')
            fh.write('t INFO x: execve(0x1 -> "/bin/sh", 0x2 -> ["sh"])\n')
            fh.write('t INFO x: DETLOG second line\n')
            path = fh.name
        try:
            ex, det = p.load(path)
            self.assertEqual(det, [' first line', ' second line'])
            self.assertIn('/bin/sh', ex)
        finally:
            os.unlink(path)

    def test_load_of_a_missing_file_returns_none_rather_than_raising(self):
        self.assertEqual(p.load('/nonexistent/definitely'), (None, None))


if __name__ == '__main__':
    unittest.main()

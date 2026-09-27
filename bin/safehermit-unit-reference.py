#!/usr/bin/env python3
"""Retain one safehermit unit until its owner finishes terminal accounting.

The parent owns the command pipe. No guest or systemctl invocation inherits the
D-Bus connection. RefUnit and UnrefUnit use that same persistent connection;
closing it also releases the reference on failure or interruption.
"""

import ctypes
import os
from pathlib import Path
import re
import select
import signal
import sys
import time


class BusError(ctypes.Structure):
    _fields_ = [
        ("name", ctypes.c_char_p),
        ("message", ctypes.c_char_p),
        ("need_free", ctypes.c_int),
    ]


class UnitReference:
    def __init__(self, unit):
        if not re.fullmatch(r"safehermit-[0-9]{8}T[0-9]{6}Z-[0-9]+\.service", unit):
            raise ValueError("invalid owned safehermit unit")
        self.unit = unit.encode()
        self.bus = ctypes.c_void_p()
        self.held = False
        self.lib = ctypes.CDLL("libsystemd.so.0")
        self.lib.sd_bus_open_user.argtypes = [ctypes.POINTER(ctypes.c_void_p)]
        self.lib.sd_bus_open_user.restype = ctypes.c_int
        self.lib.sd_bus_set_method_call_timeout.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        self.lib.sd_bus_set_method_call_timeout.restype = ctypes.c_int
        self.lib.sd_bus_call_method.argtypes = [
            ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p,
            ctypes.c_char_p, ctypes.POINTER(BusError), ctypes.POINTER(ctypes.c_void_p),
            ctypes.c_char_p,
        ]
        self.lib.sd_bus_call_method.restype = ctypes.c_int
        self.lib.sd_bus_message_unref.argtypes = [ctypes.c_void_p]
        self.lib.sd_bus_message_unref.restype = ctypes.c_void_p
        self.lib.sd_bus_error_free.argtypes = [ctypes.POINTER(BusError)]
        self.lib.sd_bus_error_free.restype = ctypes.POINTER(BusError)
        self.lib.sd_bus_flush_close_unref.argtypes = [ctypes.c_void_p]
        self.lib.sd_bus_flush_close_unref.restype = ctypes.c_void_p
        status = self.lib.sd_bus_open_user(ctypes.byref(self.bus))
        if status < 0:
            self.close()
            raise OSError(-status, os.strerror(-status))
        status = self.lib.sd_bus_set_method_call_timeout(self.bus, 1_000_000)
        if status < 0:
            self.close()
            raise OSError(-status, os.strerror(-status))

    def method(self, member):
        error = BusError()
        reply = ctypes.c_void_p()
        try:
            status = self.lib.sd_bus_call_method(
                self.bus, b"org.freedesktop.systemd1", b"/org/freedesktop/systemd1",
                b"org.freedesktop.systemd1.Manager", member,
                ctypes.byref(error), ctypes.byref(reply), b"s",
                ctypes.c_char_p(self.unit),
            )
            if status < 0:
                detail = error.message.decode(errors="replace") if error.message else os.strerror(-status)
                raise OSError(-status, detail)
        finally:
            if reply:
                self.lib.sd_bus_message_unref(reply)
            self.lib.sd_bus_error_free(ctypes.byref(error))

    def acquire(self):
        self.method(b"RefUnit")
        self.held = True

    def close(self):
        if self.bus:
            try:
                if self.held:
                    self.method(b"UnrefUnit")
            finally:
                self.lib.sd_bus_flush_close_unref(self.bus)
                self.bus = ctypes.c_void_p()
                self.held = False


def owner_start(owner):
    raw = Path("/proc", str(owner), "stat").read_text()
    return int(raw[raw.rindex(")") + 2:].split()[19])


def main():
    unit, owner_raw, lifetime_raw = sys.argv[1:]
    owner = int(owner_raw)
    original_start = owner_start(owner)
    deadline = time.monotonic() + int(lifetime_raw)

    def interrupted(signum, _frame):
        raise RuntimeError("unit reference interrupted: " + str(signum))

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, interrupted)
    reference = UnitReference(unit)
    try:
        reference.acquire()
        print("held", flush=True)
        while True:
            if time.monotonic() >= deadline or owner_start(owner) != original_start:
                raise RuntimeError("unit reference owner or lifetime ended")
            if not select.select([sys.stdin], [], [], min(0.2, deadline - time.monotonic()))[0]:
                continue
            command = os.read(sys.stdin.fileno(), 1)
            if command == b"r":
                reference.close()
                print("released", flush=True)
                return
            if command != b"c":
                raise RuntimeError("unit reference command pipe closed or malformed")
            # Check the still-open connection before launch and final publication.
            # A lost connection must not silently become a new reference/client.
            reference.method(b"GetUnit")
            print("held", flush=True)
    finally:
        reference.close()


if __name__ == "__main__":
    main()

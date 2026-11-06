#!/usr/bin/env python3
"""
Reads QEMU serial output from two Unix sockets:
  COM1 (argv[1]) --> kernel debug output (core_N.txt + all.txt)
  COM2 (argv[2]) --> userspace output (userspace_pid_<pid>.txt + all.txt)

All files are written into  logs/<YYYY-MM-DD_HH-MM-SS>/

Usage:
  python3 scripts/log_splitter.py /tmp/mofuos_com1.sock /tmp/mofuos_com2.sock
  or its just hooked up to make run
"""

import sys
import re
import socket
import datetime
import threading
from pathlib import Path

def _kernel_max_cores() -> int:
    try:
        lib = Path(__file__).parent.parent / "kernel" / "src" / "lib.rs"
        txt = lib.read_text(encoding="utf-8", errors="ignore")
        m = re.search(r"pub\s+const\s+MAX_CORES\s*:\s*u8\s*=\s*(\d+)", txt)
        if m:
            v = int(m.group(1))
            if 1 <= v <= 64:
                return v
    except Exception:
        pass
    return 4

MAX_CORES = _kernel_max_cores()

LOGS_ROOT = Path(__file__).parent.parent / "logs"

_ANSI_RE = re.compile(r'\x1b\[[0-9;]*[a-zA-Z]')
_CORE_RE = re.compile(r"^\[Core\s+(\d+)")
_PID_RE = re.compile(r"^\[pid=(\d+)\](.*)$", re.DOTALL)

def make_timestamp() -> str:
    return datetime.datetime.now().strftime("%Y-%m-%d_%H-%M-%S")

def strip_ansi(line: str) -> str:
    return _ANSI_RE.sub('', line)

class SessionFiles:
    """Holds all open log file handles for a single boot session."""

    def __init__(self, session_dir: Path, ts: str):
        self.session_dir = session_dir
        self.ts = ts
        self._lock = threading.Lock()

        session_dir.mkdir(parents=True, exist_ok=True)

        self.all_f = self._open("all.txt")
        self.core_fs = {i: self._open(f"core_{i}.txt") for i in range(MAX_CORES)}
        self.pid_fs: dict[int, object] = {}

    def _open(self, name: str):
        path = self.session_dir / name
        return open(path, "w", encoding="utf-8", buffering=1)

    def pid_file(self, pid: int):
        with self._lock:
            if pid not in self.pid_fs:
                self.pid_fs[pid] = self._open(f"userspace_pid_{pid}.txt")
            return self.pid_fs[pid]

    def write(self, f, line: str):
        f.write(line + "\n")
        f.flush()

    def close(self):
        self.all_f.close()
        for f in self.core_fs.values():
            f.close()
        for f in self.pid_fs.values():
            f.close()


def route_com1_line(line: str, sf: SessionFiles):
    sf.write(sf.all_f, f"{line}")
    print(f"{line}", flush=True)

    m = _CORE_RE.match(line)
    if m:
        core_id = int(m.group(1))
        if core_id < MAX_CORES:
            sf.write(sf.core_fs[core_id], line)


def route_com2_line(line: str, sf: SessionFiles):
    # COM2 lines are prefixed by the kernel with [pid=N] before the payload.
    # Strip all [pid=N] tags from the line
    cleaned = re.sub(r'\[pid=\d+\]', '', line).strip()
    
    pid_match = re.search(r'\[pid=(\d+)\]', line)
    if pid_match:
        pid = int(pid_match.group(1))
        sf.write(sf.all_f, cleaned)
        sf.write(sf.pid_file(pid), cleaned)
        print(cleaned, flush=True)
    else:
        # Untagged COM2 line (shouldn't happen, but don't lose it).
        sf.write(sf.all_f, f"[?] {line}")
        print(f"[?] {line}", flush=True)


def socket_lines(sock_path: str, timeout_s: float = 5.0):
    """
    Connect to a QEMU Unix socket and yield decoded lines.
    QEMU creates the socket when using  -serial unix:<path>,server
    (blocking) or  ,server,nowait.  We retry for `timeout_s` seconds
    so the script can be started before or concurrently with QEMU.
    """
    import time
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    deadline = time.monotonic() + timeout_s
    while True:
        try:
            sock.connect(sock_path)
            break
        except (FileNotFoundError, ConnectionRefusedError, OSError):
            if time.monotonic() > deadline:
                print(f"[log_splitter] ERROR: could not connect to {sock_path} "
                      f"after {timeout_s}s", file=sys.stderr)
                return
            time.sleep(0.05)

    buf = ""
    try:
        while True:
            data = sock.recv(4096)
            if not data:
                break
            buf += data.decode("utf-8", errors="replace")
            while "\n" in buf:
                line, buf = buf.split("\n", 1)
                yield line
        # yield any remaining partial line
        if buf:
            yield buf
    finally:
        sock.close()


def stdin_lines():
    for line in sys.stdin:
        yield line.rstrip("\r\n")


def main():
    ts = make_timestamp()
    session_dir = LOGS_ROOT / ts
    sf = SessionFiles(session_dir, ts)

    print(f"[log_splitter] Session:   {ts}")
    print(f"[log_splitter] Log dir:   {session_dir}/")

    if len(sys.argv) >= 3:
        # Two-socket mode: COM1 + COM2
        com1_path = sys.argv[1]
        com2_path = sys.argv[2]

        done = threading.Event()

        def read_com2():
            for line in socket_lines(com2_path, timeout_s=30.0):
                line = strip_ansi(line)
                route_com2_line(line, sf)
            done.set()

        t2 = threading.Thread(target=read_com2, daemon=True)
        t2.start()

        try:
            for line in socket_lines(com1_path, timeout_s=10.0):
                line = strip_ansi(line)
                route_com1_line(line, sf)
        except KeyboardInterrupt:
            pass
        finally:
            done.wait(timeout=1.0)

    elif len(sys.argv) == 2:
        # Single-socket mode: COM1 only
        try:
            for line in socket_lines(sys.argv[1], timeout_s=10.0):
                route_com1_line(line, sf)
        except KeyboardInterrupt:
            pass

    else:
        # stdin mode
        try:
            for line in stdin_lines():
                route_com1_line(line, sf)
        except KeyboardInterrupt:
            pass

    sf.close()
    print(f"\n[log_splitter] Session ended. Logs in {session_dir}/")


if __name__ == "__main__":
    main()

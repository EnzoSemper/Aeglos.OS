#!/usr/bin/env python3
"""
Aeglos OS Package Server — serves ELF binaries and .pkg bundles to the OS
over HTTP on port 19999 (same port as the test server, routed via SLIRP).

Usage:
    python3 tools/pkg_server.py

From inside the OS (ash shell):
    download http://10.0.2.2:19999/ash /ash_new
    download http://10.0.2.2:19999/pkg/ash.pkg /tmp.pkg
    install /tmp.pkg

Package format (.pkg):
    [0..8]   magic   b"AEGPKG\x01\x00"
    [8..12]  name_len (u32 LE)
    [12..]   name bytes
    [+0..+4] elf_len (u32 LE)
    [+4..]   elf bytes
    [+0..+8] caps (u64 LE)  — capability bitmask for the process
"""

import http.server
import os
import struct
import json
import io
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent
TARGET_DIR   = PROJECT_ROOT / "target" / "aarch64-unknown-none" / "release"
PORT         = 19999

MAGIC = b"AEGPKG\x01\x00"

# Default capability set: SEND|RECV|LOG|MEM|AI  (matches CAP_USER_DEFAULT)
CAP_USER_DEFAULT = (1<<0)|(1<<1)|(1<<4)|(1<<5)|(1<<3)

def make_pkg(name: str, elf_bytes: bytes, caps: int = CAP_USER_DEFAULT) -> bytes:
    name_b = name.encode()
    buf = io.BytesIO()
    buf.write(MAGIC)
    buf.write(struct.pack("<I", len(name_b)))
    buf.write(name_b)
    buf.write(struct.pack("<I", len(elf_bytes)))
    buf.write(elf_bytes)
    buf.write(struct.pack("<Q", caps))
    return buf.getvalue()

def available_elfs():
    """Return list of (name, path) for built ELF binaries."""
    elfs = []
    for p in TARGET_DIR.iterdir():
        if p.is_file() and not p.suffix and not p.name.endswith(".d"):
            # Skip files that look like build artifacts
            try:
                with open(p, "rb") as f:
                    magic = f.read(4)
                if magic == b"\x7fELF":
                    elfs.append((p.name, p))
            except Exception:
                pass
    return sorted(elfs)

class AeglosHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        print(f"[pkg_server] {self.address_string()} {fmt % args}")

    def do_GET(self):
        path = self.path.rstrip("/")

        # ── GET / — package listing ───────────────────────────────────────────
        if path in ("", "/", "/packages"):
            elfs = available_elfs()
            listing = {
                "packages": [
                    {
                        "name": name,
                        "elf_url":  f"/{name}",
                        "pkg_url":  f"/pkg/{name}.pkg",
                        "size":     p.stat().st_size,
                    }
                    for name, p in elfs
                ]
            }
            body = json.dumps(listing, indent=2).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # ── GET /pkg/<name>.pkg — bundled package ─────────────────────────────
        if path.startswith("/pkg/") and path.endswith(".pkg"):
            elf_name = path[5:-4]  # strip /pkg/ and .pkg
            elf_path = TARGET_DIR / elf_name
            if not elf_path.exists():
                self._404(f"ELF not found: {elf_name}")
                return
            try:
                elf_bytes = elf_path.read_bytes()
            except Exception as e:
                self._500(str(e))
                return
            pkg = make_pkg(elf_name, elf_bytes)
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Disposition", f'attachment; filename="{elf_name}.pkg"')
            self.send_header("Content-Length", str(len(pkg)))
            self.end_headers()
            self.wfile.write(pkg)
            return

        # ── GET /<name> — raw ELF binary ──────────────────────────────────────
        elf_name = path.lstrip("/")
        if "/" not in elf_name and elf_name:
            elf_path = TARGET_DIR / elf_name
            if elf_path.exists() and elf_path.is_file():
                try:
                    data = elf_path.read_bytes()
                except Exception as e:
                    self._500(str(e))
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return

        self._404(path)

    def _404(self, msg):
        body = f"Not found: {msg}\n".encode()
        self.send_response(404)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _500(self, msg):
        body = f"Error: {msg}\n".encode()
        self.send_response(500)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    elfs = available_elfs()
    print(f"[pkg_server] Aeglos Package Server on port {PORT}")
    print(f"[pkg_server] Serving {len(elfs)} ELF(s) from {TARGET_DIR}")
    for name, _ in elfs:
        print(f"  /{name}  →  /pkg/{name}.pkg")
    print(f"[pkg_server] From the OS: download http://10.0.2.2:{PORT}/ash /ash_new")
    print(f"[pkg_server] Press Ctrl-C to stop.\n")
    server = http.server.HTTPServer(("", PORT), AeglosHandler)
    server.serve_forever()

#!/usr/bin/env python3
"""Deterministic fixture for the documentation screenshots.

Two subcommands:

    shot-fixture.py seed  <download-root>
    shot-fixture.py serve <download-root> [port]

`seed` writes six historical tasks into the demo database (the daemon must
create it first) and creates the two sparse payloads the live tasks download.
`serve` is a rate-limited HTTP source: 64 KiB every 100 ms per connection,
so the default four connections settle near 2.5 MB/s and eight near 5 MB/s.
Both are fixed values on purpose: the screenshots must reproduce.
"""

import json
import os
import sqlite3
import sys
import time
import socketserver
from http.server import BaseHTTPRequestHandler

CHUNK = 64 * 1024
DELAY = 0.1
PAYLOAD_BYTES = 2 * 1024 * 1024 * 1024
LIVE = ("archive-demo.zip", "video-demo.mp4")

# (task id, filename, state, downloaded, total, completed_at_days_ago)
HISTORY = [
    ("Project-Blueprint.pdf", "project-blueprint.pdf", "completed",
     18 * 1024 * 1024, 18 * 1024 * 1024, 0),
    ("Ubuntu-24.04.1-desktop-amd64.iso", "ubuntu-24.04.1.iso", "completed",
     6 * 1024 * 1024 * 1024, 6 * 1024 * 1024 * 1024, 2),
    ("Blender-4.3.0-linux-x64.tar.xz", "blender-4.3.0.tar.xz", "completed",
     320 * 1024 * 1024, 320 * 1024 * 1024, 4),
    ("clip-library-2026-11.mp4", "clip-library.mp4", "completed",
     1400 * 1024 * 1024, 1400 * 1024 * 1024, 5),
    ("archlinux-2026.11.01-x86_64.iso", "archlinux.iso", "paused",
     500 * 1024 * 1024, 11 * 1024 * 1024 * 1024, None),
    ("data-export-2026-10.zip", "data-export.zip", "failed",
     210 * 1024 * 1024, 640 * 1024 * 1024, None),
]


def payload_path(root, name):
    return os.path.join(root, "payload", name)


def seed(root):
    database = os.path.join(root, "downloads", ".aether", "aether.db")
    if not os.path.exists(database):
        raise SystemExit(f"database not created yet: {database} (start the daemon first)")
    os.makedirs(os.path.join(root, "payload"), exist_ok=True)
    for name in LIVE:
        path = payload_path(root, name)
        if not os.path.exists(path) or os.path.getsize(path) != PAYLOAD_BYTES:
            with open(path, "wb") as handle:
                handle.truncate(PAYLOAD_BYTES)

    now = int(time.time() * 1000)
    day = 24 * 3600 * 1000
    connection = sqlite3.connect(database)
    connection.execute("DELETE FROM task_files")
    connection.execute("DELETE FROM tasks")
    for index, (task_id, filename, state, done, total, offset) in enumerate(HISTORY):
        created = now - day * (offset or 6) - index * 60_000
        completed = now - day * offset if offset is not None else None
        connection.execute(
            "INSERT INTO tasks (id, url, plugin_id, filename, state, downloaded, total,"
            " speed_bps, error_kind, error_message, error_retryable, settings,"
            " created_at, updated_at, completed_at, target_dir, retry_attempts,"
            " retry_at, priority) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                task_id,
                "http://127.0.0.1:8899/" + filename,
                "http",
                filename,
                state,
                done,
                total,
                0,
                "network" if state == "failed" else None,
                "connection reset by peer" if state == "failed" else None,
                1 if state == "failed" else 0,
                json.dumps({"connections": 4}),
                created,
                now,
                completed,
                None,
                0,
                None,
                0,
            ),
        )
        if state == "completed":
            connection.execute(
                "INSERT INTO task_files (task_id, relative_path, size, checksum)"
                " VALUES (?,?,?,NULL)",
                (task_id, filename, total),
            )
    connection.commit()
    connection.close()
    print(f"seeded {database}")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def _path(self):
        self.root = os.path.join(self.server.root, "payload")
        name = os.path.basename(self.path.split("?", 1)[0]) or "payload.bin"
        return os.path.join(self.root, name)

    def do_HEAD(self):
        path = self._path()
        if not os.path.isfile(path):
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(os.path.getsize(path)))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/octet-stream")
        self.end_headers()

    def do_GET(self):
        path = self._path()
        if not os.path.isfile(path):
            self.send_error(404)
            return
        size = os.path.getsize(path)
        start, end = 0, size - 1
        header = self.headers.get("Range")
        if header and header.startswith("bytes="):
            spec = header.split("=", 1)[1].split(",")[0]
            first, _, last = spec.partition("-")
            try:
                start = int(first) if first else 0
                end = int(last) if last else size - 1
            except ValueError:
                start, end = 0, size - 1
        end = min(end, size - 1)
        length = end - start + 1
        partial = start != 0 or end != size - 1
        self.send_response(206 if partial else 200)
        self.send_header("Content-Length", str(length))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/octet-stream")
        if partial:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        with open(path, "rb") as handle:
            handle.seek(start)
            remaining = length
            while remaining > 0:
                block = handle.read(min(CHUNK, remaining))
                if not block:
                    break
                try:
                    self.wfile.write(block)
                except (BrokenPipeError, ConnectionResetError):
                    return
                remaining -= len(block)
                time.sleep(DELAY)


class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True


def serve(root, port):
    os.makedirs(os.path.join(root, "payload"), exist_ok=True)
    server = Server(("127.0.0.1", port), Handler)
    server.root = root
    print(f"serving {root}/payload on 127.0.0.1:{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    root = os.environ.get("AETHER_SHOT_ROOT", "/tmp/aether-shots")
    if command == "seed":
        seed(root)
    elif command == "serve":
        serve(root, int(sys.argv[2]) if len(sys.argv) > 2 else 8899)
    else:
        raise SystemExit(__doc__)

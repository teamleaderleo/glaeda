#!/usr/bin/env python3
"""glaeda-gh's own GitHub client (the daemon's http(), which no gh shim fronts): a dead api.github.com edge costs one
3 s connect timeout, then the request goes to another edge.

On 2026-11-17 from 22:40Z one host resolved api.github.com only to one edge, which dropped most SYNs from there and
answered the rest with a 403 that carried no rate-limit headers; the daemon's reads failed with URLError every tick.
Edge A here is what the system resolver gives: it drops every SYN (a listener whose one-slot accept queue is full,
so Linux drops further SYNs) or answers a bare 403. Edge B is what a second resolver (a fake DNS-over-HTTPS server)
names, and answers 200. Both speak TLS for api.github.com with a throwaway certificate trusted through SSL_CERT_FILE.
Linux only: the fakes need 127.0.0.2 and 127.0.0.3.
"""
from __future__ import annotations

import http.server
import importlib.machinery
import importlib.util
import json
import os
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
import http.server as _hs; _hs.HTTPServer.server_bind = lambda s: (_hs.socketserver.TCPServer.server_bind(s), setattr(s, "server_name", str(s.server_address[0])), setattr(s, "server_port", s.server_address[1])) and None  # no reverse DNS on bind (Local Network prompt): scripts/test-python-http-server-no-lookup.py

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_gh_edge", os.fspath(ROOT / "scripts" / "glaeda-gh"))
spec = importlib.util.spec_from_loader("glaeda_gh_edge", loader)
gg = importlib.util.module_from_spec(spec)
sys.modules["glaeda_gh_edge"] = gg
loader.exec_module(gg)

GOOD_IP = "127.0.0.2"   # edge B: only the second resolver names it
BAD_IP = "127.0.0.3"    # edge A: what the system resolver gives


class Edge:
    """A fake api.github.com edge at IP:PORT. mode: ok (200 with rate-limit headers), drop (every SYN dropped),
    403 (a bare 403, as the bad edge answered), 403rl (a 403 with rate-limit headers: GitHub's own refusal)."""

    def __init__(self, ip: str, port: int, mode: str, cert: Path, key: Path):
        self.ip, self.mode, self.hits = ip, mode, 0
        self.sock = socket.socket()
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((ip, port))
        self.port = self.sock.getsockname()[1]
        self.fill: list[socket.socket] = []
        if mode == "drop":
            self.sock.listen(0)
            for _ in range(8):  # one queued connection fills the queue; from then on the kernel drops SYNs
                c = socket.socket()
                c.settimeout(0.3)
                self.fill.append(c)
                try:
                    c.connect((ip, self.port))
                except OSError:
                    break
            return
        self.sock.listen(16)
        self.ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.ctx.load_cert_chain(cert, key)
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self) -> None:
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            self.hits += 1
            threading.Thread(target=self.answer, args=(conn,), daemon=True).start()

    def answer(self, conn: socket.socket) -> None:
        try:
            conn.settimeout(5)
            tls = self.ctx.wrap_socket(conn, server_side=True)
            buf = b""
            while True:
                while b"\r\n\r\n" not in buf:
                    data = tls.recv(4096)
                    if not data:
                        return
                    buf += data
                head, _, buf = buf.partition(b"\r\n\r\n")
                path = head.split(b" ")[1].decode()
                if self.mode == "ok":
                    status, extra = "200 OK", "X-RateLimit-Limit: 60\r\nX-RateLimit-Remaining: 59\r\n"
                elif self.mode == "403rl":
                    status, extra = "403 Forbidden", "X-RateLimit-Limit: 60\r\nX-RateLimit-Remaining: 0\r\n"
                else:
                    status, extra = "403 Forbidden", ""
                body = json.dumps({"edge": self.ip, "path": path}).encode()
                tls.sendall(f"HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}"
                            f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        except (OSError, ssl.SSLError, IndexError):
            pass
        finally:
            conn.close()

    def close(self) -> None:
        for c in self.fill:
            c.close()
        self.sock.close()


class DoH(http.server.ThreadingHTTPServer):
    """A second resolver: DNS-over-HTTPS JSON (the JSON /resolve shape) that names edge B."""

    def __init__(self, answer: str):
        self.answer, self.asked = answer, 0
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                outer.asked += 1
                answer = [{"name": "api.github.com.", "type": 1, "data": outer.answer}] if outer.answer else []
                body = json.dumps({"Status": 0, "Answer": answer})
                self.send_response(200)
                self.send_header("Content-Type", "application/dns-json")
                self.end_headers()
                self.wfile.write(body.encode())

            def log_message(self, *args):
                pass

        super().__init__(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.serve_forever, daemon=True).start()


@unittest.skipUnless(sys.platform.startswith("linux") and shutil.which("openssl"), "needs Linux loopback aliases and openssl")
class EdgeRetry(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp(prefix="glaeda-edge-test."))
        cls.cert, cls.key = cls.tmp / "cert.pem", cls.tmp / "key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(cls.key),
                        "-out", str(cls.cert), "-days", "2", "-subj", "/CN=api.github.com",
                        "-addext", "subjectAltName=DNS:api.github.com"], check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def setUp(self):
        self.good = Edge(GOOD_IP, 0, "ok", self.cert, self.key)
        self.doh = DoH(GOOD_IP)
        self.bad = None
        self.logged: list[dict] = []
        self.saved_env = dict(os.environ)
        for name in ("HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "GH_EDGE_PROXY"):
            os.environ.pop(name, None)
        os.environ.update(SSL_CERT_FILE=str(self.cert), GLAEDA_EDGE_TEST_RESOLVE=f"api.github.com={BAD_IP}",
                          GLAEDA_EDGE_TEST_PORT=str(self.good.port),
                          GLAEDA_EDGE_DOH=f"http://127.0.0.1:{self.doh.server_address[1]}/resolve")
        self.saved_log = gg.log
        gg.log = self.logged.append
        if hasattr(gg, "EDGE"):
            gg.EDGE.reset()

    def tearDown(self):
        gg.log = self.saved_log
        os.environ.clear()
        os.environ.update(self.saved_env)
        self.good.close()
        self.doh.shutdown()
        self.doh.server_close()
        if self.bad:
            self.bad.close()

    def retries(self) -> list[dict]:
        return [e for e in self.logged if e.get("event") == "edge-retry"]

    def zen(self) -> tuple[int, dict]:
        status, _, body = gg.http("GET", "https://api.github.com/zen", {})
        return status, json.loads(body)

    def test_dropped_syn_retries_on_the_second_resolvers_edge_within_4s(self):
        self.bad = Edge(BAD_IP, self.good.port, "drop", self.cert, self.key)
        t = time.monotonic()
        status, body = self.zen()
        took = time.monotonic() - t
        self.assertEqual((status, body["edge"]), (200, GOOD_IP))
        self.assertGreater(took, 2.5)  # the 3 s connect timeout on the first edge, no more
        self.assertLess(took, 4.5)
        [line] = self.retries()
        self.assertEqual((line["from"], line["to"], line["why"]), (BAD_IP, GOOD_IP, "connect-timeout"))
        t = time.monotonic()  # remembered: the next request goes straight to edge B
        self.assertEqual(self.zen()[1]["edge"], GOOD_IP)
        self.assertLess(time.monotonic() - t, 1.5)

    def test_a_lone_edge_marked_bad_is_still_dialled_when_no_other_is_known(self):
        # One blip marked the only resolved edge bad and the second resolver knows no other: reads still go to it,
        # rather than every read failing for EDGE_BAD_TTL.
        self.bad = Edge(BAD_IP, self.good.port, "ok", self.cert, self.key)
        self.doh.answer = None
        gg.EDGE.mark_bad(BAD_IP)
        self.assertEqual(self.zen(), (200, {"edge": BAD_IP, "path": "/zen"}))

    def test_a_bare_403_with_no_other_edge_is_returned_as_before(self):
        self.bad = Edge(BAD_IP, self.good.port, "403", self.cert, self.key)
        self.doh.answer = None
        self.assertEqual(self.zen()[0], 403)

    def test_bare_403_retries_on_another_edge(self):
        self.bad = Edge(BAD_IP, self.good.port, "403", self.cert, self.key)
        status, body = self.zen()
        self.assertEqual((status, body["edge"]), (200, GOOD_IP))
        [line] = self.retries()
        self.assertEqual((line["from"], line["to"], line["why"]), (BAD_IP, GOOD_IP, "edge-403"))

    def test_rate_limited_403_is_not_retried(self):
        self.bad = Edge(BAD_IP, self.good.port, "403rl", self.cert, self.key)
        status, body = self.zen()
        self.assertEqual((status, body["edge"]), (403, BAD_IP))
        self.assertEqual(self.good.hits, 0)
        self.assertEqual(self.retries(), [])


class Classify(unittest.TestCase):
    def test_only_a_bare_403_is_the_edges(self):
        self.assertTrue(gg.edge_refused(403, {}))
        self.assertFalse(gg.edge_refused(403, {"x-ratelimit-remaining": "0"}))
        self.assertFalse(gg.edge_refused(403, {"Retry-After": "60"}))
        self.assertFalse(gg.edge_refused(200, {}))


if __name__ == "__main__":
    unittest.main()

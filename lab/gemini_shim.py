#!/usr/bin/env python3
"""Gemini-protocol ⇄ OpenAI-compatible translation shim for the hxr lab.

gemini-cli (Google) only speaks the Gemini wire protocol: you can point it at
a custom base URL via GOOGLE_GEMINI_BASE_URL, but that endpoint must answer
`…/models/<m>:generateContent` / `:streamGenerateContent?alt=sse`. The lab's
LAN endpoint (deep, sglang) serves the OpenAI chat-completions protocol.

This shim listens on localhost (plain HTTP — gemini-cli allows non-HTTPS only
for localhost), translates, and logs every request path so adapter
archaeology can see exactly what the CLI asked for.

Usage (inside the lab container):
    python3 /src/lab/gemini_shim.py &          # listens on :8423
    GOOGLE_GEMINI_BASE_URL=http://localhost:8423 GEMINI_API_KEY=shim \
        gemini -p "hello" -m <DEEP_MODEL>
"""

import json
import os
import sys
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM = os.environ.get("DEEP_BASE_URL", "http://192.168.1.99:8001/v1")
MODEL_FALLBACK = os.environ.get("DEEP_MODEL", "DeepSeek-V4-Flash-0731")
PORT = int(os.environ.get("SHIM_PORT", "8423"))
LOG = os.environ.get("SHIM_LOG", "/tmp/gemini-shim.log")


def log(msg: str) -> None:
    with open(LOG, "a") as f:
        f.write(msg + "\n")


def openai_chat(body: dict) -> dict:
    """POST /chat/completions upstream, return the parsed JSON."""
    messages = []
    if (si := body.get("systemInstruction")) and (
        text := "".join(p.get("text", "") for p in si.get("parts", []))
    ):
        messages.append({"role": "system", "content": text})
    for c in body.get("contents", []):
        text = "".join(
            p.get("text", "") for p in c.get("parts", []) if "text" in p
        )
        role = "assistant" if c.get("role") == "model" else "user"
        if text:
            messages.append({"role": role, "content": text})
    req = urllib.request.Request(
        f"{UPSTREAM}/chat/completions",
        data=json.dumps(
            {"model": MODEL_FALLBACK, "messages": messages, "stream": False}
        ).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=300) as r:
        return json.load(r)


def gemini_chunk(text: str, finish: str | None = None) -> dict:
    cand = {
        "content": {"parts": [{"text": text}], "role": "model"},
        "index": 0,
    }
    if finish:
        cand["finishReason"] = finish
        cand.pop("index", None)
    chunk = {"candidates": [cand]}
    if finish:
        chunk["usageMetadata"] = {}
    return chunk


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # keep stdout quiet
        pass

    def _send(self, code: int, payload: dict | list, sse_events: list[dict] | None = None):
        if sse_events is not None:
            # Gemini SSE: one data: event per chunk; the stream ends when a
            # chunk carries finishReason (no OpenAI-style [DONE] sentinel)
            parts = []
            for ev in sse_events:
                parts.append(f"data: {json.dumps(ev)}\r\n\r\n")
            body = "".join(parts).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
        else:
            body = json.dumps(payload).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        log(f"GET {self.path}")
        if self.path.split("?")[0].endswith("/models"):
            self._send(
                200,
                {"models": [{"name": f"models/{MODEL_FALLBACK}"}]},
            )
        else:
            self._send(404, {"error": {"message": f"no GET {self.path}"}})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        path = self.path.split("?")[0]
        log(f"POST {self.path} contents={len(body.get('contents', []))}")
        try:
            if path.endswith(":listModels"):
                self._send(200, {"models": [{"name": f"models/{MODEL_FALLBACK}"}]})
                return
            resp = openai_chat(body)
        except Exception as e:  # upstream error → Gemini-shaped error
            log(f"  upstream error: {e}")
            self._send(502, {"error": {"code": 502, "message": str(e)}})
            return
        text = resp["choices"][0]["message"].get("content") or ""
        if "streamGenerateContent" in path:
            # one text chunk followed by a finishReason chunk closes the
            # stream cleanly for the SDK's SSE parser
            self._send(200, None, sse_events=[
                gemini_chunk(text),
                gemini_chunk("", finish="STOP"),
            ])
        else:
            out = gemini_chunk(text)
            out["candidates"][0]["finishReason"] = "STOP"
            self._send(200, out)


if __name__ == "__main__":
    log(f"--- shim start: {UPSTREAM} model={MODEL_FALLBACK} port={PORT}")
    print(f"gemini shim → {UPSTREAM} on :{PORT}", file=sys.stderr)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()

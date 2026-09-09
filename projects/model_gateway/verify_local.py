#!/usr/bin/env python3
"""Exercise the live IRIS gateway against an ephemeral local HTTP fixture."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iris", default="iris")
    args = parser.parse_args()
    compiler = str(Path(args.iris).resolve()) if Path(args.iris).exists() else args.iris
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append((self.path, body))
            reply = json.dumps({"choices": [{"message": {"content": "status"}}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    env = os.environ.copy()
    env.pop("IRIS_FORCE_INTERP", None)
    env.pop("IRIS_LLM_KEY", None)
    env["IRIS_CHAT_URL"] = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
    env["IRIS_MODEL"] = "fixture-model"
    try:
        with tempfile.TemporaryDirectory(prefix="iris-gateway-") as work:
            output = subprocess.run([compiler, "run", str(Path(__file__).with_name("live.iris"))], cwd=work, env=env, capture_output=True, text=True, timeout=90)
        assert output.returncode == 0, output.stdout + output.stderr
        assert "action=status" in output.stdout, output.stdout
        assert len(requests) == 1, requests
        path, body = requests[0]
        assert path == "/v1/chat/completions", path
        assert body["model"] == "fixture-model", body
        assert body["messages"][1]["content"] == "Show service health.", body
        print("live gateway: request, model, messages, response and action verified")
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


if __name__ == "__main__":
    main()

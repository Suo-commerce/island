#!/usr/bin/env python3
"""A scripted model: an OpenAI-compatible chat endpoint whose answers follow
from the conversation it is sent, so flare-agent can be driven without a
GPU (docs/slice-2.md, "Harness and gates"). Used by `just agent-test`
(imported) and by the server gate (run as a program).

    scripted_model.py --listen 127.0.0.1:PORT [--requests FILE]

What it answers, by the last line of the newest message from Jarmo:

    ... fork ...             as the prime: the tool call `fork` with two
                             tasks; then, reading the children's notes,
                             `promote` of the last child that is done
                             (refused a fork, it says why).
                             As a fork child (told so by fork's result):
                             `write` of draft-N.md, a `verse` if its task
                             asks for one, then what it made
    ... propose a verse ...  the tool call `verse` (the usual words, or those
                             after "saying"), then that it did
    anything else            `write` of hello.txt holding that line, then
                             that it did
    ... slowly ...           the same, each answer after --slow seconds
                             (4), so a gate can kill something meanwhile

With --requests, each request's body is appended to FILE as one JSON line,
when it arrives.
"""

import argparse
import http.server
import json
import re
import threading
import time

FORK_TASKS = ["Draft it plainly, in draft-1.md", "Draft it as a list, in draft-2.md, with a verse"]


def tool_call(name, args, call_id):
    return [{"choices": [{"delta": {"tool_calls": [
        {"index": 0, "id": call_id, "function": {"name": name, "arguments": json.dumps(args)}}]}}]},
        {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}]


def text(content):
    return [{"choices": [{"delta": {"content": content}}]},
            {"choices": [{"delta": {}, "finish_reason": "stop"}]}]


def answer(body):
    """The chunks of the answer to a chat request."""
    messages = body["messages"]
    users = [i for i, m in enumerate(messages) if m["role"] == "user"]
    asked = messages[users[-1]]["content"].splitlines()[-1]
    since = messages[users[-1] + 1:]
    last = messages[-1]
    results = [m.get("content") or "" for m in since if m["role"] == "tool"]
    child = next((r for r in results if r.startswith("You are now a fork child")), None)

    if child is not None:
        number = int(re.search(r"child (\d+) of \d+", child).group(1))
        task = child.split("Your task:\n", 1)[1].split("\n\n", 1)[0]
        done = len(results) - 1          # tool calls answered since it became a child
        if done == 0:
            return tool_call("write", {"path": f"draft-{number}.md", "content": task}, f"call-w{number}")
        if done == 1 and "with a verse" in task:
            return tool_call("verse", {"evidence": "decision",
                                       "text": f"Fork child {number} drafted it as a list"}, f"call-v{number}")
        return text(f"Child {number} drafted draft-{number}.md.")

    if last["role"] == "tool":
        notes = last.get("content") or ""
        if notes.startswith("Forked "):
            done = re.findall(r"^## (\S+) \(task \d+\), done:", notes, re.M)
            if done:
                return tool_call("promote", {"child": done[-1]}, "call-p")
            return text("No child finished; nothing was promoted.")
        if notes.startswith(("not forked", "forking went wrong")):
            return text("I could not fork: " + notes.split(". ")[0] + ".")
        if "propose a verse" in asked:
            return text("\n\nI proposed a verse; it waits for you.")
        return text(f"I wrote hello.txt for: {asked}")
    if "fork" in asked:
        return tool_call("fork", {"tasks": FORK_TASKS}, "call-f")
    if "propose a verse" in asked:
        # "... propose a verse saying WORDS": that verse; else the usual one.
        words = asked.split(" saying ", 1)[1].strip() if " saying " in asked else "Verses from the agent wait for approval"
        return tool_call("verse", {"evidence": "decision", "text": words}, "call-v")
    return tool_call("write", {"path": "hello.txt", "content": asked}, "call-1")


class ScriptedModel(http.server.BaseHTTPRequestHandler):
    """The endpoint. `requests` holds every request body; `log` names a
    file they are appended to."""

    requests = []
    log = None
    slow = 4.0
    lock = threading.Lock()

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        with ScriptedModel.lock:
            ScriptedModel.requests.append(body)
            if ScriptedModel.log:
                with open(ScriptedModel.log, "a") as out:
                    out.write(json.dumps(body) + "\n")
        users = [m["content"] for m in body["messages"] if m["role"] == "user"]
        if users and "slowly" in users[-1].splitlines()[-1]:
            time.sleep(ScriptedModel.slow)
        payload = "".join(f"data: {json.dumps(c)}\n\n" for c in answer(body)) + "data: [DONE]\n\n"
        data = payload.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    p = argparse.ArgumentParser(description="a scripted OpenAI-compatible model")
    p.add_argument("--listen", required=True, help="HOST:PORT")
    p.add_argument("--requests", help="append each request body to this file")
    p.add_argument("--slow", type=float, default=4.0, help="seconds before an answer asked for slowly")
    args = p.parse_args()
    host, _, port = args.listen.rpartition(":")
    ScriptedModel.log = args.requests
    ScriptedModel.slow = args.slow
    server = http.server.ThreadingHTTPServer((host, int(port)), ScriptedModel)
    print(f"scripted-model: listening on {args.listen}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

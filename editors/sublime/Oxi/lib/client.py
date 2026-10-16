"""Talks to `oxi headless` over stdio: one JSON object per line each way. `oxi headless`
relays to the oxi daemon that every editor shares, so notifications may be about chats this
editor did not start.

No `sublime` import: callbacks go through `schedule`, which the plugin points at
`sublime.set_timeout` so they run on the UI thread in arrival order.
"""

import json
import os
import subprocess
import threading


class Client:
    def __init__(self, binary, schedule, on_notification, on_exit, log=None):
        self._schedule = schedule
        self._on_notification = on_notification
        self._on_exit = on_exit
        self._log = log or (lambda text: None)
        self._lock = threading.Lock()
        self._next_id = 1
        self._pending = {}
        startupinfo = None
        if os.name == "nt":
            startupinfo = subprocess.STARTUPINFO()
            startupinfo.dwFlags |= subprocess.STARTF_USESHOWWINDOW
        self.proc = subprocess.Popen(
            [binary, "headless"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=os.path.expanduser("~"),
            startupinfo=startupinfo,
        )
        threading.Thread(target=self._read_stdout, name="oxi-stdout", daemon=True).start()
        threading.Thread(target=self._read_stderr, name="oxi-stderr", daemon=True).start()

    def alive(self):
        return self.proc.poll() is None

    def request(self, method, params=None, callback=None):
        """Send a request; `callback(result, error)` runs on the UI thread with the answer."""
        with self._lock:
            request_id = self._next_id
            self._next_id += 1
            if callback is not None:
                self._pending[request_id] = callback
        line = json.dumps({"id": request_id, "method": method, "params": params or {}})
        try:
            self.proc.stdin.write(line.encode("utf-8") + b"\n")
            self.proc.stdin.flush()
        except (OSError, ValueError) as e:
            with self._lock:
                self._pending.pop(request_id, None)
            if callback is not None:
                self._schedule(lambda: callback(None, "oxi is not running ({})".format(e)))
        return request_id

    def stop(self):
        try:
            self.request("shutdown")
            self.proc.wait(timeout=2)
        except Exception:
            pass
        if self.alive():
            self.proc.kill()

    def _read_stdout(self):
        for raw in self.proc.stdout:
            try:
                message = json.loads(raw.decode("utf-8"))
            except ValueError:
                self._log("oxi: unreadable line: {!r}".format(raw[:200]))
                continue
            if "method" in message:
                method, params = message["method"], message.get("params") or {}
                self._schedule(lambda m=method, p=params: self._on_notification(m, p))
                continue
            with self._lock:
                callback = self._pending.pop(message.get("id"), None)
            if callback is not None:
                result, error = message.get("result"), message.get("error")
                self._schedule(lambda c=callback, r=result, e=error: c(r, e))
        with self._lock:
            pending, self._pending = self._pending, {}
        for callback in pending.values():
            self._schedule(lambda c=callback: c(None, "oxi stopped"))
        self._schedule(self._on_exit)

    def _read_stderr(self):
        for raw in self.proc.stderr:
            self._log(raw.decode("utf-8", "replace").rstrip())

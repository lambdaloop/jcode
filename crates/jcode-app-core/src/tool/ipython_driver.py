"""jcode IPython driver: a line-protocol REPL over a private fd.

Protocol (JSON lines). Host -> driver on stdin:
  {"type": "exec", "code": str}
  {"type": "reply", "id": int, "ok": bool, "output": str, "error": str}
Driver -> host on the protocol fd (the original stdout):
  {"type": "ready", "python": str, "version": str}
  {"type": "call", "id": int, "tool": str, "args": dict}
  {"type": "result", "stdout": str, "stderr": str, "result": str|None,
   "error": {...}|None, "images": [{"mime": str, "data": b64}]}
User code writes to fd 1/2 go to stderr, which the host drains separately.
"""
import base64
import json
import os
import signal
import sys

_proto = os.fdopen(os.dup(1), "w", buffering=1, encoding="utf-8")
os.dup2(2, 1)
_stdin = sys.stdin
sys.stdin = open(os.devnull)


def _send(msg):
    _proto.write(json.dumps(msg) + "\n")
    _proto.flush()


_call_id = 0


def _host_call(tool, args):
    """Run a jcode tool from the kernel and return its text output."""
    global _call_id
    _call_id += 1
    _send({"type": "call", "id": _call_id, "tool": tool, "args": args})
    while True:
        line = _stdin.readline()
        if not line:
            raise RuntimeError("jcode host closed the kernel channel")
        msg = json.loads(line)
        if msg.get("type") == "reply" and msg.get("id") == _call_id:
            if not msg.get("ok"):
                raise RuntimeError(msg.get("error") or "jcode tool call failed")
            return msg.get("output", "")


from IPython.core.interactiveshell import InteractiveShell  # noqa: E402
from IPython.utils.capture import capture_output  # noqa: E402

shell = InteractiveShell.instance(colors="NoColor")
shell.display_formatter.active_types = ["text/plain", "image/png", "image/jpeg", "image/gif", "image/webp"]
shell.ex(
    "import sys as _jc_sys\n"
    f"_jc_sys.path.insert(0, {os.path.dirname(os.path.abspath(__file__))!r})\n"
)
shell.user_ns["_jcode_host_call"] = _host_call
shell.ex("from jcode_ipython_helpers import *")
sys.modules["jcode_ipython_helpers"]._jcode_host_call = _host_call
try:
    import matplotlib

    matplotlib.use("Agg")
except Exception:
    pass

_IMAGE_TYPES = ("image/png", "image/jpeg", "image/gif", "image/webp")


def _run(code):
    images = []
    error = None
    with capture_output() as cap:
        res = shell.run_cell(code, store_history=True)
    for out in cap.outputs:
        for mime in _IMAGE_TYPES:
            if mime in out.data:
                data = out.data[mime]
                if isinstance(data, bytes):
                    data = base64.b64encode(data).decode("ascii")
                images.append({"mime": mime, "data": data})
                break
    err = res.error_before_exec or res.error_in_exec
    stderr = cap.stderr
    if err is not None:
        error = {"ename": type(err).__name__, "evalue": str(err)}
        if isinstance(err, KeyboardInterrupt):
            error["ename"] = "KeyboardInterrupt"
    result = None
    if res.result is not None:
        try:
            result = repr(res.result)
        except Exception as exc:  # pragma: no cover
            result = f"<unrepresentable: {exc}>"
    return {
        "type": "result",
        "stdout": cap.stdout,
        "stderr": stderr,
        "result": result,
        "error": error,
        "images": images,
    }


def main():
    signal.signal(signal.SIGINT, signal.default_int_handler)
    _send({"type": "ready", "python": sys.executable, "version": sys.version.split()[0]})
    while True:
        try:
            line = _stdin.readline()
            if not line:
                return
            msg = json.loads(line)
            if msg.get("type") != "exec":
                continue
            _send(_run(msg.get("code", "")))
        except KeyboardInterrupt:
            # An interrupt that lands between cells: report and keep serving.
            _send({"type": "result", "stdout": "", "stderr": "", "result": None,
                   "error": {"ename": "KeyboardInterrupt", "evalue": ""}, "images": []})


if __name__ == "__main__":
    main()

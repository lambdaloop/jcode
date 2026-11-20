"""Helpers preloaded into the jcode ipython kernel.

Ported from pi-ipython-subagents' pi_rlm_runtime: show_image, rg_files,
rg_search, plus thin `swarm` and `bg` adapters that call jcode's native
tools through the host channel.
"""
from __future__ import annotations

import base64
import io
import os
import shlex
import shutil
import subprocess
from typing import Any, Iterable

__all__ = ["show_image", "rg_files", "rg_search", "swarm", "bg", "jcode_tool"]

_RG_TIMEOUT_SECONDS = 60
_RG_MAX_LINE_CHARS = 400


class FileList(list):
    def __init__(self, paths: Iterable[str] = (), truncated: bool = False, command: str = ""):
        super().__init__(paths)
        self.truncated = truncated
        self.command = command

    def __str__(self) -> str:
        lines = list(self)
        if self.truncated:
            lines.append("… output truncated; narrow with path= or glob=")
        return "\n".join(lines) or "(no files found)"

    __repr__ = __str__


class SearchResult(str):
    def __new__(cls, matches: Iterable[str] = (), truncated: bool = False, command: str = ""):
        lines = tuple(matches)
        rendered = "\n".join(lines)
        if truncated:
            rendered += "\n… output truncated; narrow with path= or glob="
        obj = super().__new__(cls, rendered)
        obj.matches = lines
        obj.truncated = truncated
        obj.command = command
        return obj

    def __str__(self) -> str:
        return str.__str__(self) or "(no matches)"

    __repr__ = __str__


def _search_path(path: str) -> str:
    if not isinstance(path, str) or not path.strip():
        raise ValueError("path must be a non-empty string")
    expanded = os.path.expanduser(path)
    resolved = os.path.realpath(expanded)
    if resolved in {"/", "/home", os.path.realpath(os.path.expanduser("~"))}:
        raise ValueError(f"Scope the search to the project or a subdirectory; {resolved} is too large")
    return expanded


def _run_rg(args: list[str], limit: int):
    binary = shutil.which("rg")
    if not binary:
        raise RuntimeError("ripgrep (rg) is not installed or not on PATH")
    command = shlex.join([binary, *args])
    try:
        res = subprocess.run([binary, *args], capture_output=True, text=True, errors="replace",
                             timeout=_RG_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as e:
        raise RuntimeError(f"rg timed out after {_RG_TIMEOUT_SECONDS}s; narrow the search") from e
    if res.returncode not in (0, 1):
        raise RuntimeError(res.stderr.strip() or f"rg exited with status {res.returncode}")
    raw = res.stdout.splitlines()
    return [l[:_RG_MAX_LINE_CHARS] for l in raw[:limit]], len(raw) > limit, command


def rg_files(glob: str | None = None, path: str = ".", *, hidden: bool = False,
             no_ignore: bool = False, max_files: int = 200) -> FileList:
    """List project files with rg --files, bounded for model context."""
    args = ["--files"]
    if hidden:
        args.append("--hidden")
    if no_ignore:
        args.append("--no-ignore")
    if glob:
        args += ["-g", glob]
    args += ["--", _search_path(path)]
    return FileList(*_run_rg(args, max_files))


def rg_search(pattern: str, path: str = ".", *, glob: str | None = None, literal: bool = False,
              ignore_case: bool = False, context: int = 0, max_matches: int = 100,
              hidden: bool = False, no_ignore: bool = False) -> SearchResult:
    """Search project contents with rg, bounded for model context."""
    if not pattern:
        raise ValueError("pattern must be a non-empty string")
    args = ["--line-number", "--no-heading", "--color", "never"]
    for flag, on in (("--hidden", hidden), ("--no-ignore", no_ignore),
                     ("--fixed-strings", literal), ("--ignore-case", ignore_case)):
        if on:
            args.append(flag)
    if context:
        args += ["-C", str(int(context))]
    if glob:
        args += ["-g", glob]
    args += ["--", pattern, _search_path(path)]
    return SearchResult(*_run_rg(args, max_matches))


def _image_mime(data: bytes) -> str | None:
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if data.startswith(b"\xff\xd8\xff"):
        return "image/jpeg"
    if data[:6] in (b"GIF87a", b"GIF89a"):
        return "image/gif"
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "image/webp"
    return None


def _encode_pil(image: Any, max_size: int | None):
    if max_size is not None and max(image.size) > max_size:
        image = image.copy()
        image.thumbnail((max_size, max_size))
    buf = io.BytesIO()
    if image.mode in ("RGBA", "LA", "P"):
        image.save(buf, format="PNG")
        return buf.getvalue(), "image/png"
    if image.mode not in ("RGB", "L"):
        image = image.convert("RGB")
    image.save(buf, format="JPEG", quality=90)
    return buf.getvalue(), "image/jpeg"


def show_image(image: Any, *, max_size: int | None = 1568) -> str:
    """Attach an image (path, bytes, PIL image, or matplotlib figure) to the cell result."""
    label = "image"
    data = None
    pil = None
    if hasattr(image, "savefig"):
        buf = io.BytesIO()
        image.savefig(buf, format="png", bbox_inches="tight")
        data, label = buf.getvalue(), "matplotlib figure"
    elif hasattr(image, "save") and hasattr(image, "size") and hasattr(image, "mode"):
        pil, label = image, "PIL image"
    elif isinstance(image, (bytes, bytearray, memoryview)):
        data = bytes(image)
    else:
        label = os.path.expanduser(os.fspath(image))
        with open(label, "rb") as f:
            data = f.read()
    mime = _image_mime(data) if data is not None else None
    if pil is None and mime is not None and max_size is not None:
        try:
            from PIL import Image
            with Image.open(io.BytesIO(data)) as probe:
                if max(probe.size) > max_size and not getattr(probe, "is_animated", False):
                    pil = probe.copy()
        except Exception:
            pass
    if pil is not None or mime is None:
        if pil is None:
            try:
                from PIL import Image
            except ImportError as e:
                raise ValueError("show_image supports PNG, JPEG, GIF, WebP without Pillow") from e
            pil = Image.open(io.BytesIO(data))
        data, mime = _encode_pil(pil, max_size)
    from IPython.display import publish_display_data

    desc = f"[{label}: {mime}, {len(data)} bytes]"
    publish_display_data({mime: base64.b64encode(data).decode("ascii"), "text/plain": desc})
    return desc


def jcode_tool(name: str, **args: Any) -> str:
    """Call any jcode tool by name from the kernel; returns its text output."""
    args.setdefault("intent", f"ipython {name}")
    return _jcode_host_call(name, args)  # noqa: F821 (injected by the driver)


def swarm(action: str, **kwargs: Any) -> str:
    """Thin adapter to jcode's native `swarm` tool. kwargs are its exact schema.

    Example:
        swarm("spawn", label="auth-review",
              prompt="Review the authorization flow and report findings.")
        swarm("await_members", mode="all", timeout_minutes=30)
        swarm("message", to_session="auth-review", message="Check tests next.")
    """
    return jcode_tool("swarm", action=action, **kwargs)


class _Bg:
    """Background shell tasks wired into jcode's bg tool: bg("pytest", name="tests")."""

    def __call__(self, command: str, name: str | None = None, timeout_seconds: int | None = None) -> str:
        args: dict[str, Any] = {"command": command, "run_in_background": True,
                                "intent": name or command[:60]}
        if timeout_seconds:
            args["timeout"] = int(timeout_seconds) * 1000
        return jcode_tool("bash", **args)

    def list(self) -> str:
        return jcode_tool("bg", action="list")

    def status(self, task_id: str) -> str:
        return jcode_tool("bg", action="status", task_id=task_id)

    def logs(self, task_id: str, tail_lines: int = 80) -> str:
        return jcode_tool("bg", action="tail", task_id=task_id, tail_lines=tail_lines)

    def kill(self, task_id: str) -> str:
        return jcode_tool("bg", action="cancel", task_id=task_id)

    def wait(self, task_id: str, timeout_seconds: int = 60) -> str:
        return jcode_tool("bg", action="wait", task_id=task_id, max_wait_seconds=timeout_seconds)


bg = _Bg()

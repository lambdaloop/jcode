//! Persistent IPython kernel tool.
//!
//! Each session gets one long-lived Python process running
//! `ipython_driver.py`, a JSON line-protocol REPL built on
//! `IPython.core.interactiveshell`. Variables survive across calls. Cells are
//! interrupted (SIGINT) when they exceed their timeout. Kernel code may call
//! back into jcode tools (`rlm`, `agent_message`, `bg`, `jcode_tool`) through
//! the same channel, which the host serves with this session's registry.

use super::{Tool, ToolContext, ToolOutput, WeakRegistry};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

const DRIVER_PY: &str = include_str!("ipython_driver.py");
const HELPERS_PY: &str = include_str!("jcode_ipython_helpers.py");
const DEFAULT_TIMEOUT_SECS: u64 = 20;
const MAX_TIMEOUT_SECS: u64 = 3600;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
const INTERRUPT_GRACE: Duration = Duration::from_secs(3);
const MAX_OUTPUT_CHARS: usize = 30_000;
pub(crate) const UV_DEFAULT_PACKAGES: &[&str] = &[
    "ipykernel",
    "numpy",
    "pandas",
    "matplotlib",
    "pillow",
    "requests",
    "scipy",
    "pyyaml",
    "tabulate",
];
/// Tools kernel code may not call back into.
const FORBIDDEN_CALLBACK_TOOLS: &[&str] = &["ipython", "batch", "codemode"];

const DESCRIPTION: &str = "Run Python in a persistent IPython kernel for this session. \
Variables, imports, and state persist across calls. Cells are interrupted after 20s by default \
(set timeout_seconds for slower cells). Prefer for data work, quick scripts, plotting, and \
multi-step analysis. Preloaded: show_image(path|fig|PIL|bytes) attaches an image to the result; \
rg_files(glob, path) and rg_search(pattern, path, glob=...) are bounded ripgrep helpers; \
rlm(task, name=...) spawns a jcode subagent (rlm.status(), rlm.wait(), rlm.delete_subagent(id)); \
agent_message.send(msg, receiver_name=...); bg(command, name=..., timeout_seconds=...) starts a \
jcode background task (bg.list/status/logs/wait/kill); jcode_tool(name, **args) calls any tool. \
Put `%%kernel` on the first line to list environments, or `%%kernel pixi [-e ENV]`, \
`%%kernel uv [--with PKG ...] [--no-default-packages]`, `%%kernel python /path/to/python` to \
restart in another interpreter (state is discarded); code after the directive runs in the new kernel. \
Default env: $JCODE_IPYTHON_PYTHON, else the project's pixi env, else uv Python 3.11 with numpy, \
pandas, matplotlib, pillow, requests, scipy, pyyaml, tabulate. Do not pip install into the kernel.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KernelSpec {
    Python(PathBuf),
    Pixi {
        root: PathBuf,
        env: String,
    },
    Uv {
        extra: Vec<String>,
        no_defaults: bool,
    },
}

impl KernelSpec {
    fn describe(&self) -> String {
        match self {
            KernelSpec::Python(p) => format!("python {}", p.display()),
            KernelSpec::Pixi { root, env } => format!("pixi -e {env} ({})", root.display()),
            KernelSpec::Uv { extra, no_defaults } => {
                format!("uv python 3.11 with {}", uv_packages(extra, *no_defaults).join(", "))
            }
        }
    }
}

fn uv_packages(extra: &[String], no_defaults: bool) -> Vec<String> {
    let mut pkgs: Vec<String> = if no_defaults {
        vec!["ipykernel".into()]
    } else {
        UV_DEFAULT_PACKAGES.iter().map(|s| s.to_string()).collect()
    };
    for p in extra {
        if !pkgs.contains(p) {
            pkgs.push(p.clone());
        }
    }
    pkgs
}

/// Parsed `%%kernel` first-line directive.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KernelDirective {
    List,
    Switch(KernelSpec),
}

pub(crate) fn find_pixi_env(start: &Path, env: &str) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if dir.join("pixi.toml").exists() || dir.join(".pixi").is_dir() {
            let py = dir.join(".pixi/envs").join(env).join("bin/python");
            if py.exists() {
                return Some(dir.to_path_buf());
            }
        }
    }
    None
}

/// Split a cell into an optional directive and the remaining code.
pub(crate) fn parse_directive(
    code: &str,
    cwd: &Path,
) -> Result<(Option<KernelDirective>, String)> {
    let trimmed = code.trim_start_matches(['\n', '\r']);
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    let first = first.trim();
    if !first.starts_with("%%kernel") {
        return Ok((None, code.to_string()));
    }
    let args = shell_words(first.trim_start_matches("%%kernel"));
    let rest = rest.to_string();
    let Some(kind) = args.first() else {
        return Ok((Some(KernelDirective::List), rest));
    };
    let spec = match kind.as_str() {
        "list" => return Ok((Some(KernelDirective::List), rest)),
        "pixi" => {
            let mut env = "default".to_string();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "-e" || a == "--environment" {
                    env = it.next().cloned().ok_or_else(|| anyhow!("-e needs a name"))?;
                } else if let Some(v) = a.strip_prefix("--environment=") {
                    env = v.to_string();
                }
            }
            let root = find_pixi_env(cwd, &env).ok_or_else(|| {
                anyhow!(
                    "No materialized pixi environment '{env}' found above {} (run `pixi install`).",
                    cwd.display()
                )
            })?;
            KernelSpec::Pixi { root, env }
        }
        "uv" => {
            let mut extra = Vec::new();
            let mut no_defaults = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--with" {
                    extra.push(it.next().cloned().ok_or_else(|| anyhow!("--with needs a package"))?);
                } else if let Some(v) = a.strip_prefix("--with=") {
                    extra.push(v.to_string());
                } else if a == "--no-default-packages" {
                    no_defaults = true;
                } else {
                    extra.push(a.clone());
                }
            }
            KernelSpec::Uv { extra, no_defaults }
        }
        "python" => {
            let p = args.get(1).ok_or_else(|| anyhow!("%%kernel python needs a path"))?;
            KernelSpec::Python(PathBuf::from(p))
        }
        other => bail!("Unknown %%kernel target '{other}'. Use list, pixi, uv, or python."),
    };
    Ok((Some(KernelDirective::Switch(spec)), rest))
}

fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub(crate) fn default_spec(cwd: &Path) -> KernelSpec {
    if let Ok(p) = std::env::var("JCODE_IPYTHON_PYTHON")
        && !p.trim().is_empty()
    {
        return KernelSpec::Python(PathBuf::from(p));
    }
    if let Some(root) = find_pixi_env(cwd, "default") {
        let py = root.join(".pixi/envs/default/bin/python");
        let ok = std::process::Command::new(&py)
            .args(["-c", "import IPython"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return KernelSpec::Pixi {
                root,
                env: "default".into(),
            };
        }
    }
    KernelSpec::Uv {
        extra: Vec::new(),
        no_defaults: false,
    }
}

fn import_to_package(module: &str) -> String {
    let top = module.split('.').next().unwrap_or(module);
    match top {
        "PIL" => "pillow",
        "cv2" => "opencv-python",
        "yaml" => "pyyaml",
        "sklearn" => "scikit-learn",
        "bs4" => "beautifulsoup4",
        "skimage" => "scikit-image",
        "dateutil" => "python-dateutil",
        "dotenv" => "python-dotenv",
        "serial" => "pyserial",
        "Crypto" => "pycryptodome",
        "attr" => "attrs",
        "docx" => "python-docx",
        "pptx" => "python-pptx",
        "fitz" => "pymupdf",
        "magic" => "python-magic",
        other => other,
    }
    .to_string()
}

pub(crate) fn missing_module_hint(evalue: &str, spec: &KernelSpec) -> Option<String> {
    let start = evalue.find("No module named '")? + "No module named '".len();
    let module = &evalue[start..start + evalue[start..].find('\'')?];
    let pkg = import_to_package(module);
    let mut s = format!(
        "\nHint: '{module}' is not installed in this kernel. Do not `pip install` into it.\n"
    );
    match spec {
        KernelSpec::Uv { .. } => s.push_str(&format!(
            "  - For this session: run `%%kernel uv --with {pkg}` as the first line of a cell (restarts the kernel).\n"
        )),
        KernelSpec::Pixi { env, .. } => s.push_str(&format!(
            "  - This kernel runs in the project's pixi environment ({env}); add it with `pixi add {pkg}`.\n  - Or switch: `%%kernel uv --with {pkg}` (state is lost).\n"
        )),
        KernelSpec::Python(p) => s.push_str(&format!(
            "  - This kernel runs {}; install '{pkg}' there, or switch: `%%kernel uv --with {pkg}` (state is lost).\n",
            p.display()
        )),
    }
    s.push_str("  - If it is your project's own code, check the working directory and sys.path.");
    Some(s)
}

fn driver_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
        .ok_or_else(|| anyhow!("no cache directory"))?;
    let dir = base.join("jcode").join("ipython");
    std::fs::create_dir_all(&dir)?;
    for (name, body) in [
        ("ipython_driver.py", DRIVER_PY),
        ("jcode_ipython_helpers.py", HELPERS_PY),
    ] {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(body) {
            let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
            std::fs::write(&tmp, body)?;
            std::fs::rename(&tmp, &path)?;
        }
    }
    Ok(dir)
}

fn spawn_command(spec: &KernelSpec, driver: &Path) -> Command {
    let mut cmd = match spec {
        KernelSpec::Python(p) => Command::new(p),
        KernelSpec::Pixi { root, env } => {
            let mut c = Command::new("pixi");
            c.arg("run")
                .arg("--manifest-path")
                .arg(root)
                .arg("-e")
                .arg(env)
                .arg("python");
            c
        }
        KernelSpec::Uv { extra, no_defaults } => {
            let mut c = Command::new("uv");
            c.args(["run", "--no-project", "--python", "3.11"]);
            for p in uv_packages(extra, *no_defaults) {
                c.arg("--with").arg(p);
            }
            c.arg("python");
            c
        }
    };
    cmd.arg("-u").arg(driver);
    cmd
}

struct Kernel {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<StdMutex<String>>,
    spec: KernelSpec,
    python: String,
    version: String,
}

impl Kernel {
    async fn start(spec: KernelSpec, cwd: Option<&Path>) -> Result<Self> {
        let dir = driver_dir()?;
        let mut cmd = spawn_command(&spec, &dir.join("ipython_driver.py"));
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.env("PYTHONUNBUFFERED", "1")
            .env("MPLBACKEND", "Agg")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to start kernel ({})", spec.describe()))?;
        let stdin = child.stdin.take().context("kernel stdin")?;
        let mut lines = BufReader::new(child.stdout.take().context("kernel stdout")?).lines();
        let stderr = Arc::new(StdMutex::new(String::new()));
        let mut err_pipe = child.stderr.take().context("kernel stderr")?;
        let sink = stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            while let Ok(n) = err_pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut s = sink.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > 200_000 {
                    let cut = s.len() - 100_000;
                    let cut = (cut..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                    s.drain(..cut);
                }
            }
        });
        let ready = tokio::time::timeout(STARTUP_TIMEOUT, async {
            while let Some(line) = lines.next_line().await? {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && v["type"] == "ready"
                {
                    return Ok::<_, anyhow::Error>(Some(v));
                }
            }
            Ok(None)
        })
        .await;
        let ready = match ready {
            Ok(Ok(Some(v))) => v,
            other => {
                let _ = child.start_kill();
                let log = stderr.lock().unwrap().clone();
                let why = match other {
                    Err(_) => "timed out".to_string(),
                    Ok(Err(e)) => e.to_string(),
                    _ => "exited".to_string(),
                };
                bail!(
                    "IPython kernel failed to start ({}): {why}\n{}",
                    spec.describe(),
                    tail(&log, 4000)
                );
            }
        };
        stderr.lock().unwrap().clear();
        Ok(Self {
            child,
            stdin,
            lines,
            stderr,
            python: ready["python"].as_str().unwrap_or("").to_string(),
            version: ready["version"].as_str().unwrap_or("").to_string(),
            spec,
        })
    }

    fn interrupt(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // SAFETY: plain kill(2) on our own child pid.
            unsafe {
                libc::kill(pid as i32, libc::SIGINT);
            }
        }
    }

    async fn send(&mut self, v: Value) -> Result<()> {
        let mut s = serde_json::to_string(&v)?;
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

fn tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = s.len() - max;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

type KernelSlot = Arc<Mutex<Option<Kernel>>>;
static KERNELS: LazyLock<StdMutex<HashMap<String, KernelSlot>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));
/// Per-session environment chosen with `%%kernel`, reused on restarts.
static SESSION_SPECS: LazyLock<StdMutex<HashMap<String, KernelSpec>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

fn slot(session: &str) -> KernelSlot {
    KERNELS
        .lock()
        .unwrap()
        .entry(session.to_string())
        .or_default()
        .clone()
}

#[derive(Deserialize)]
struct IpythonInput {
    code: String,
    #[serde(default)]
    timeout_seconds: Option<f64>,
}

pub struct IpythonTool {
    registry: WeakRegistry,
}

impl IpythonTool {
    pub(super) fn new(registry: WeakRegistry) -> Self {
        Self { registry }
    }

    async fn serve_call(&self, v: &Value, ctx: &ToolContext) -> Value {
        let id = v["id"].clone();
        let tool = v["tool"].as_str().unwrap_or("").to_string();
        let args = v["args"].clone();
        let result: Result<String> = async {
            if FORBIDDEN_CALLBACK_TOOLS.contains(&tool.as_str()) {
                bail!("tool '{tool}' cannot be called from the ipython kernel");
            }
            let registry = self
                .registry
                .upgrade()
                .ok_or_else(|| anyhow!("tool registry unavailable"))?;
            let sub = ctx.for_subcall(format!("ipython-{tool}-{id}"));
            let out = registry.execute(&tool, args, sub).await?;
            Ok(out.output)
        }
        .await;
        match result {
            Ok(output) => json!({"type": "reply", "id": id, "ok": true, "output": output}),
            Err(e) => json!({"type": "reply", "id": id, "ok": false, "error": format!("{e:#}")}),
        }
    }
}

fn list_environments(cwd: &Path, active: Option<&KernelSpec>) -> String {
    let mut s = String::from("Kernel environments:\n");
    if let Some(a) = active {
        s.push_str(&format!("  active: {}\n", a.describe()));
    } else {
        s.push_str("  active: (not started)\n");
    }
    if let Ok(p) = std::env::var("JCODE_IPYTHON_PYTHON") {
        s.push_str(&format!("  JCODE_IPYTHON_PYTHON={p}\n"));
    }
    match find_pixi_env(cwd, "default") {
        Some(root) => {
            let envs = std::fs::read_dir(root.join(".pixi/envs"))
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            s.push_str(&format!(
                "  pixi: {} (envs: {envs}); `%%kernel pixi [-e ENV]`\n",
                root.display()
            ));
        }
        None => s.push_str("  pixi: no materialized pixi environment in this project\n"),
    }
    s.push_str(&format!(
        "  uv: python 3.11 with {}; `%%kernel uv [--with PKG] [--no-default-packages]`\n",
        UV_DEFAULT_PACKAGES.join(", ")
    ));
    s.push_str("  python: `%%kernel python /abs/path/to/python` (needs IPython)\n");
    s
}

#[async_trait]
impl Tool for IpythonTool {
    fn name(&self) -> &str {
        "ipython"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "code": {"type": "string", "description": "Python cell. Optional first-line %%kernel directive."},
                "timeout_seconds": {"type": "number", "description": "Cell time limit in seconds (default 20). Use bg(...) for long work."}
            },
            "required": ["code"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: IpythonInput = serde_json::from_value(input)?;
        let cwd = ctx
            .working_dir
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let timeout_secs = params
            .timeout_seconds
            .filter(|t| t.is_finite() && *t > 0.0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS as f64)
            .min(MAX_TIMEOUT_SECS as f64);
        let (directive, code) = parse_directive(&params.code, &cwd)?;
        let slot = slot(&ctx.session_id);
        let mut guard = slot.lock().await;
        let mut preface = String::new();

        match directive {
            Some(KernelDirective::List) => {
                preface = list_environments(&cwd, guard.as_ref().map(|k| &k.spec));
            }
            Some(KernelDirective::Switch(spec)) => {
                // Verify the new env before replacing the old kernel.
                let kernel = Kernel::start(spec.clone(), Some(&cwd)).await?;
                preface = format!(
                    "Kernel switched to {} (Python {}, {}). Previous state discarded.\n",
                    spec.describe(),
                    kernel.version,
                    kernel.python
                );
                SESSION_SPECS
                    .lock()
                    .unwrap()
                    .insert(ctx.session_id.clone(), spec);
                *guard = Some(kernel);
            }
            None => {}
        }
        if code.trim().is_empty() {
            return Ok(ToolOutput::new(preface.trim_end().to_string()).with_title("%%kernel"));
        }

        let alive = guard
            .as_mut()
            .is_some_and(|k| matches!(k.child.try_wait(), Ok(None)));
        if !alive {
            let restarted = guard.is_some();
            let spec = SESSION_SPECS
                .lock()
                .unwrap()
                .get(&ctx.session_id)
                .cloned()
                .unwrap_or_else(|| default_spec(&cwd));
            let kernel = Kernel::start(spec, Some(&cwd)).await?;
            if restarted {
                preface.push_str("[kernel restarted; previous state lost]\n");
            }
            *guard = Some(kernel);
        }
        let kernel = guard.as_mut().expect("kernel started");
        kernel.stderr.lock().unwrap().clear();
        kernel.send(json!({"type": "exec", "code": code})).await?;

        let started = Instant::now();
        let deadline = started + Duration::from_secs_f64(timeout_secs);
        let mut interrupted_at: Option<Instant> = None;
        let result: Option<Value> = loop {
            let wait_until = interrupted_at.map(|t| t + INTERRUPT_GRACE).unwrap_or(deadline);
            let next = tokio::time::timeout_at(wait_until.into(), kernel.lines.next_line()).await;
            match next {
                Ok(Ok(Some(line))) => {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    match v["type"].as_str() {
                        Some("result") => break Some(v),
                        Some("call") => {
                            let reply = self.serve_call(&v, &ctx).await;
                            kernel.send(reply).await?;
                        }
                        _ => {}
                    }
                }
                Ok(Ok(None)) | Ok(Err(_)) => break None,
                Err(_) if interrupted_at.is_none() => {
                    kernel.interrupt();
                    interrupted_at = Some(Instant::now());
                }
                Err(_) => break None,
            }
        };
        let elapsed = started.elapsed().as_secs_f64();
        let fd_output = std::mem::take(&mut *kernel.stderr.lock().unwrap());
        let spec = kernel.spec.clone();

        let Some(res) = result else {
            // Kernel died or ignored the interrupt: drop it so the next call restarts.
            if let Some(mut k) = guard.take() {
                let _ = k.child.start_kill();
            }
            let mut out = preface;
            out.push_str(&fd_output);
            out.push_str(&format!(
                "\nIPython kernel {} after {elapsed:.1}s; state was reset. Re-run with a larger timeout_seconds or use bg(\"...\") for long work.",
                if interrupted_at.is_some() { "did not respond to interrupt" } else { "exited" }
            ));
            return Ok(ToolOutput::new(out).with_title(format!("ipython · {elapsed:.1}s")));
        };

        let mut out = preface;
        for key in ["stdout", "stderr"] {
            if let Some(s) = res[key].as_str() {
                out.push_str(s);
            }
        }
        out.push_str(&fd_output);
        if let Some(r) = res["result"].as_str() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("Out: {r}\n"));
        }
        if let Some(err) = res["error"].as_object() {
            let ename = err.get("ename").and_then(Value::as_str).unwrap_or("");
            let evalue = err.get("evalue").and_then(Value::as_str).unwrap_or("");
            if ename == "KeyboardInterrupt" && interrupted_at.is_some() {
                out.push_str(&format!(
                    "\nIPython cell exceeded its {timeout_secs:.0}s time limit and was interrupted. Re-run with timeout_seconds=<n> for a longer cell, or start it with bg(\"...\") for genuinely long work."
                ));
            } else if ename == "ModuleNotFoundError"
                && let Some(hint) = missing_module_hint(evalue, &spec)
            {
                out.push_str(&hint);
            }
        }
        if out.chars().count() > MAX_OUTPUT_CHARS {
            let kept = tail(&out, MAX_OUTPUT_CHARS).to_string();
            out = format!("[output truncated to last {MAX_OUTPUT_CHARS} bytes]\n{kept}");
        }
        if out.trim().is_empty() {
            out = "(no output)".into();
        }
        let mut output = ToolOutput::new(out)
            .with_title(format!("ipython · {elapsed:.1}s"))
            .with_metadata(json!({"elapsed_seconds": elapsed, "kernel": spec.describe()}));
        if let Some(images) = res["images"].as_array() {
            for img in images {
                if let (Some(mime), Some(data)) = (img["mime"].as_str(), img["data"].as_str()) {
                    output = output.with_image(mime, data);
                }
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
#[path = "ipython_tests.rs"]
mod tests;

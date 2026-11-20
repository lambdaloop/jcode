use super::*;
use crate::tool::Registry;
use jcode_tool_core::ToolExecutionMode;

#[test]
fn directive_parsing() {
    let cwd = std::env::temp_dir();
    assert_eq!(parse_directive("x = 1", &cwd).unwrap(), (None, "x = 1".into()));
    let (d, rest) = parse_directive("%%kernel\nprint(1)", &cwd).unwrap();
    assert_eq!(d, Some(KernelDirective::List));
    assert_eq!(rest, "print(1)");
    let (d, _) = parse_directive("%%kernel uv --with scikit-learn --with \"pandas<2\"", &cwd).unwrap();
    assert_eq!(
        d,
        Some(KernelDirective::Switch(KernelSpec::Uv {
            extra: vec!["scikit-learn".into(), "pandas<2".into()],
            no_defaults: false
        }))
    );
    let (d, _) = parse_directive("%%kernel python /usr/bin/python3", &cwd).unwrap();
    assert_eq!(
        d,
        Some(KernelDirective::Switch(KernelSpec::Python("/usr/bin/python3".into())))
    );
    assert!(parse_directive("%%kernel bogus", &cwd).is_err());
}

#[test]
fn pixi_env_discovery() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pixi.toml"), "").unwrap();
    let bin = dir.path().join(".pixi/envs/dev/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("python"), "").unwrap();
    let sub = dir.path().join("src");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(find_pixi_env(&sub, "dev").as_deref(), Some(dir.path()));
    assert!(find_pixi_env(&sub, "default").is_none());
    let (d, _) = parse_directive("%%kernel pixi -e dev", &sub).unwrap();
    assert!(matches!(d, Some(KernelDirective::Switch(KernelSpec::Pixi { env, .. })) if env == "dev"));
}

#[test]
fn module_hint_maps_package() {
    let uv = KernelSpec::Uv { extra: vec![], no_defaults: false };
    let hint = missing_module_hint("No module named 'PIL'", &uv).unwrap();
    assert!(hint.contains("%%kernel uv --with pillow"));
    assert!(missing_module_hint("other", &uv).is_none());
    let uvp = uv_packages(&["numpy".into(), "torch".into()], false);
    assert_eq!(uvp.iter().filter(|p| *p == "numpy").count(), 1);
    assert!(uvp.contains(&"torch".to_string()));
}

/// Live kernel test; runs only when JCODE_IPYTHON_TEST_PYTHON points at a
/// Python with IPython installed.
#[tokio::test]
async fn live_kernel_persists_times_out_and_shows_images() {
    let Ok(py) = std::env::var("JCODE_IPYTHON_TEST_PYTHON") else {
        return;
    };
    let registry = Registry::empty();
    let tool = IpythonTool::new(registry.downgrade());
    let session = format!("ipython-test-{}", std::process::id());
    let ctx = || ToolContext {
        session_id: session.clone(),
        message_id: "m".into(),
        tool_call_id: "t".into(),
        working_dir: Some(std::env::temp_dir()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    };
    let run = |code: String, t: Option<f64>| {
        let tool = &tool;
        let c = ctx();
        async move {
            tool.execute(json!({"code": code, "timeout_seconds": t}), c)
                .await
                .unwrap()
        }
    };
    let out = run(format!("%%kernel python {py}\nx = 41"), None).await;
    assert!(out.output.contains("Kernel switched"), "{}", out.output);
    let out = run("print(x + 1)\nx".into(), None).await;
    assert!(out.output.contains("42"), "{}", out.output);
    assert!(out.output.contains("Out: 41"), "{}", out.output);
    let out = run("import time\ntime.sleep(30)".into(), Some(1.0)).await;
    assert!(out.output.contains("time limit"), "{}", out.output);
    let out = run("x".into(), None).await;
    assert!(out.output.contains("41"), "state survives interrupt: {}", out.output);
    let out = run("import nonexistent_pkg_zz".into(), None).await;
    assert!(out.output.contains("Hint:"), "{}", out.output);
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    let out = run(
        format!("import base64\nshow_image(base64.b64decode('{png}'), max_size=None)"),
        None,
    )
    .await;
    assert_eq!(out.images.len(), 1, "{}", out.output);
    let out = run("print(len(rg_files(path='.', max_files=3)) <= 3)".into(), None).await;
    assert!(out.output.contains("True") || out.output.contains("rg"), "{}", out.output);
    let out = run("jcode_tool('nope')".into(), None).await;
    assert!(out.output.contains("RuntimeError") || out.output.contains("Unknown"), "{}", out.output);
}

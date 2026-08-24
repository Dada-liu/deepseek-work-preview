use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

/// DSH child process handle plus its serving URL once ready.
struct DshProcess {
    child: Child,
    url: Arc<Mutex<Option<String>>>,
    /// PID file recording this child so a later launch can clean up a stale
    /// process that still locks the runtime directory (Windows error 5).
    pid_path: PathBuf,
}

impl Drop for DshProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.pid_path);
    }
}

/// Hide the console window for a spawned helper process (Windows only).
/// Any console-subsystem executable spawned from our GUI process gets a
/// visible console without this.
#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Windows-only hint for an ERROR_ACCESS_DENIED (5). The two usual causes are
/// antivirus holding the freshly-extracted runtime, and a leftover node.exe
/// from a previous session still locking its image — neither reads well as a
/// bare "os error 5".
#[cfg(windows)]
fn access_denied_hint(e: &std::io::Error) -> &'static str {
    if e.raw_os_error() == Some(5) {
        "；通常是杀软拦截，或上一个 DSH 进程(node.exe)仍残留占用——请将安装目录与 %APPDATA%/com.deepseek-harness.desktop 加入杀软白名单，并在任务管理器中结束残留的 node.exe"
    } else {
        ""
    }
}

#[cfg(not(windows))]
fn access_denied_hint(_e: &std::io::Error) -> &'static str {
    ""
}

/// Kill a stale DSH process recorded in `runtime/dsh.pid` from a previous
/// session (e.g. the app was closed to the tray or crashed). Only the exact
/// PID we recorded is touched.
fn kill_stale_dsh(runtime: &Path) {
    let pid_file = runtime.join("dsh.pid");
    let Ok(pid) = fs::read_to_string(&pid_file) else { return };
    let pid = pid.trim();
    if pid.is_empty() {
        return;
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", pid, "/F", "/T"]);
        hide_console(&mut cmd);
        let _ = cmd.status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").arg(pid).status();
    }
    // Give the OS a moment to release the file locks held by the now-dead
    // process image (Windows locks a running exe and its loaded DLLs).
    std::thread::sleep(std::time::Duration::from_secs(1));
    let _ = fs::remove_file(&pid_file);
}

/// Shared application state.
#[derive(Default)]
struct AppState {
    dsh: Mutex<Option<DshProcess>>,
    /// True once the bundled runtime has been extracted this session.
    runtime_ready: Mutex<bool>,
}

/// Snapshot exposed to the frontend.
#[derive(Serialize, Clone)]
struct DshStatus {
    running: bool,
    url: Option<String>,
}

/// Send a plain status line to the loading screen.
fn status(app: &AppHandle, message: &str) {
    let _ = app.emit("boot-status", message.to_string());
}

/// Log file location: ~/deepseek-work-preview/deepseek-work.log
fn log_file_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .home_dir()
        .ok()
        .map(|h| h.join("deepseek-work-preview").join("deepseek-work.log"))
}

/// Append one timestamped error line to the log file, creating the directory
/// if needed. All IO failures are ignored — logging must never break the app.
fn log_error(app: &AppHandle, msg: &str) {
    use std::io::Write;
    let Some(path) = log_file_path(app) else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(file, "[{}] {}", stamp, msg);
    }
}

/// Append an error reported by the injected webview filter script to the log
/// file. The script runs inside the DSH Web UI page, where it cannot touch the
/// filesystem directly, so it forwards messages here over IPC.
#[tauri::command]
fn log_frontend_error(app: AppHandle, msg: String) {
    log_error(&app, &format!("市场过滤脚本: {}", msg));
}

/// Report the DSH subprocess state.
#[tauri::command]
fn get_dsh_status(state: State<'_, AppState>) -> DshStatus {
    let guard = state.dsh.lock().unwrap();
    match guard.as_ref() {
        Some(proc) => DshStatus {
            running: true,
            url: proc.url.lock().unwrap().clone(),
        },
        None => DshStatus {
            running: false,
            url: None,
        },
    }
}

/// Start the DSH web subprocess if it is not already running.
/// This is idempotent and safe to call from the frontend at any time.
#[tauri::command]
async fn start_dsh_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if let Err(e) = ensure_runtime(&app, &state) {
        log_error(&app, &format!("运行环境初始化失败: {}", e));
        return Err(e);
    }
    let mut guard = state.dsh.lock().map_err(|e| e.to_string())?;
    if let Some(proc) = guard.as_ref() {
        // The frontend may have finished loading after the ready event fired
        // (slow vite first-serve in dev). Re-emit so it can catch up.
        if let Some(url) = proc.url.lock().unwrap().clone() {
            let _ = app.emit("dsh-ready", url);
        }
        return Ok(());
    }
    match spawn_dsh_web(&app) {
        Ok(proc) => {
            *guard = Some(proc);
            Ok(())
        }
        Err(e) => {
            log_error(&app, &format!("启动 DSH 失败: {}", e));
            Err(e)
        }
    }
}

/// Stop the DSH web subprocess.
#[tauri::command]
async fn stop_dsh_service(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.dsh.lock().map_err(|e| e.to_string())?;
    guard.take();
    Ok(())
}

/// Restart the DSH web subprocess and re-navigate the window.
#[tauri::command]
async fn restart_dsh_service(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if let Err(e) = ensure_runtime(&app, &state) {
        log_error(&app, &format!("运行环境初始化失败: {}", e));
        return Err(e);
    }
    let mut guard = state.dsh.lock().map_err(|e| e.to_string())?;
    guard.take();
    let proc = match spawn_dsh_web(&app) {
        Ok(proc) => proc,
        Err(e) => {
            log_error(&app, &format!("重启 DSH 失败: {}", e));
            return Err(e);
        }
    };
    let url = proc.url.lock().unwrap().clone();
    *guard = Some(proc);
    Ok(format!("DSH 服务已重启，URL: {}", url.unwrap_or_default()))
}

/// Return the application version from Cargo.
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The bundled Node binary name differs by platform.
fn node_bin_name() -> &'static str {
    if cfg!(windows) {
        "node.exe"
    } else {
        "node"
    }
}

/// Directory the bundled runtime is extracted to (writable).
fn runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("runtime"))
}

/// The runtime shipped inside the app bundle (read-only).
/// In debug builds the bundle layout does not exist yet, so fall back to the
/// source tree prepared by scripts/prepare-runtime.sh.
fn bundled_runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let bundled = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("runtime");
    if bundled.join(node_bin_name()).exists() {
        return Ok(bundled);
    }
    if cfg!(debug_assertions) {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime");
        if dev.join(node_bin_name()).exists() {
            return Ok(dev);
        }
    }
    Err("未找到内嵌运行环境，请先执行 scripts/prepare-runtime.sh".to_string())
}

/// Recursively copy a directory tree (fallback when APFS clone copy fails).
fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("创建目录失败 ({}): {}{}", dst.display(), e, access_denied_hint(&e)))?;
    for entry in fs::read_dir(src).map_err(|e| format!("读取目录失败 ({}): {}", src.display(), e))? {
        let entry = entry.map_err(|e| format!("读取目录项失败 ({}): {}", src.display(), e))?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| {
                format!("复制文件失败 ({} -> {}): {}{}", from.display(), to.display(), e, access_denied_hint(&e))
            })?;
        }
    }
    Ok(())
}

/// Remove a directory tree, retrying a few times — Windows briefly locks
/// files while antivirus scanners or a just-killed process release them.
fn remove_dir_all_retry(dir: &Path) -> Result<(), String> {
    // Antivirus scanning the ~450 MB runtime, or a leftover node.exe still
    // releasing its file locks, can hold the directory for seconds — retry
    // long enough to ride both out before giving up.
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..10 {
        match fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt < 9 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    }
    let e = last_err.expect("retry loop runs at least once");
    Err(format!(
        "删除旧运行环境失败 ({}): {} — 请从托盘彻底退出 DeepSeek Work 后重试{}",
        dir.display(),
        e,
        access_denied_hint(&e),
    ))
}

/// Extract the bundled runtime (Node.js + DSH) into the app data dir on
/// first launch. A marker file carrying the DSH version marks completion;
/// a version change (app upgrade) triggers a fresh extraction.
fn ensure_runtime(app: &AppHandle, state: &State<AppState>) -> Result<PathBuf, String> {
    let mut ready = state.runtime_ready.lock().map_err(|e| e.to_string())?;
    if *ready {
        return runtime_dir(app);
    }

    let bundled = bundled_runtime_dir(app)?;
    let wanted = fs::read_to_string(bundled.join("dsh-version"))
        .map_err(|e| format!("内嵌运行环境缺少版本标记: {}", e))?;
    let dir = runtime_dir(app)?;
    let marker = dir.join("dsh-version");

    let extracted = fs::read_to_string(&marker)
        .map(|m| m == wanted)
        .unwrap_or(false)
        && dir.join(node_bin_name()).exists();
    if !extracted {
        status(app, "首次启动，正在准备运行环境…");
        if dir.exists() {
            // A stale DSH from a previous session may still hold files in
            // the runtime dir (Windows reports that as "access denied").
            kill_stale_dsh(&dir);
            remove_dir_all_retry(&dir)?;
        }
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建数据目录失败 ({}): {}", parent.display(), e))?;
        }
        // macOS: prefer APFS clonefile copy (a few seconds for ~450 MB).
        // Other platforms (and non-APFS volumes) use a plain recursive copy.
        #[cfg(target_os = "macos")]
        let cloned = Command::new("cp")
            .arg("-Rc")
            .arg(&bundled)
            .arg(&dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        #[cfg(not(target_os = "macos"))]
        let cloned = false;
        if !cloned {
            copy_dir_all(&bundled, &dir)?;
        }
        // Make sure the Node binary is executable (Unix) and not quarantined
        // (macOS Gatekeeper).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(
                dir.join(node_bin_name()),
                fs::Permissions::from_mode(0o755),
            );
            // The bundled npm/pnpm launchers sit next to the Node binary and
            // are spawned bare by dsh-market, so they need the exec bit too
            // (a plain recursive copy does not preserve it).
            for shim in ["npm", "pnpm"] {
                let _ = fs::set_permissions(dir.join(shim), fs::Permissions::from_mode(0o755));
            }
        }
        #[cfg(target_os = "macos")]
        let _ = Command::new("xattr")
            .arg("-dr")
            .arg("com.apple.quarantine")
            .arg(&dir)
            .status();
    }

    *ready = true;
    Ok(dir)
}

/// The preinstalled dsh-market plugin (npm package name).
const DSH_MARKET_PACKAGE: &str = "dshmarket";
/// The bundle list dsh's own initProfile writes for a fresh web profile.
const WEB_PROFILE_DEFAULT_BUNDLES: [&str; 2] =
    ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];
/// The empty user patch layer dsh writes on profile init (PROFILE_PATCH_TEMPLATE).
const PROFILE_PATCH_TEMPLATE: &str = "# Your patch layer for this dsh profile, applied after every bundle layer:\n# a top-level YAML array of loader patch entries (id-targeted config\n# overrides, disables, and insert lists; `!!js` expressions allowed).\n[]\n";
/// The pnpm settings dsh writes on profile init (PROFILE_PNPM_WORKSPACE),
/// needed so the market's own one-click installs behave the same here.
const PROFILE_PNPM_WORKSPACE: &str =
    "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n";

/// Seed the preinstalled dsh-market plugin into the web profile.
///
/// The plugin package ships inside the bundled runtime (registered in the
/// bundled dsh copy's dependency closure, so dsh's boot-time module fallback
/// links it into `$DSH_HOME/profiles/node_modules`); what remains here is
/// listing it in the profile's `dsh.profile.bundles`. Only a fresh profile
/// or an untouched default manifest is modified — a bundles list the user
/// has customized (or deliberately removed the market from) is left alone.
fn seed_dsh_market(app: &AppHandle) -> Result<(), String> {
    let home = app.path().home_dir().map_err(|e| e.to_string())?;
    // Mirror dsh's home resolution ($DSH_HOME, then ~/.dsh).
    let dsh_home = std::env::var_os("DSH_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".dsh"));
    let dir = dsh_home.join("profiles").join("web");
    let manifest_path = dir.join("package.json");

    if !manifest_path.exists() {
        fs::create_dir_all(&dir)
            .map_err(|e| format!("创建 profile 目录失败 ({}): {}", dir.display(), e))?;
        let manifest = serde_json::json!({
            "name": "dsh-profile-web",
            "private": true,
            "dependencies": {},
            "dsh": { "profile": { "bundles": [
                WEB_PROFILE_DEFAULT_BUNDLES[0],
                WEB_PROFILE_DEFAULT_BUNDLES[1],
                DSH_MARKET_PACKAGE,
            ] } }
        });
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())? + "\n",
        )
        .map_err(|e| format!("写入 profile manifest 失败: {}", e))?;
        fs::write(dir.join("cordis.patch.yml"), PROFILE_PATCH_TEMPLATE)
            .map_err(|e| format!("写入 cordis.patch.yml 失败: {}", e))?;
        fs::write(dir.join("pnpm-workspace.yaml"), PROFILE_PNPM_WORKSPACE)
            .map_err(|e| format!("写入 pnpm-workspace.yaml 失败: {}", e))?;
        return Ok(());
    }

    let text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("读取 profile manifest 失败: {}", e))?;
    let mut manifest: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 profile manifest 失败: {}", e))?;
    let is_untouched_default = manifest
        .pointer("/dsh/profile/bundles")
        .and_then(|v| v.as_array())
        .map(|b| {
            b.len() == WEB_PROFILE_DEFAULT_BUNDLES.len()
                && b.iter()
                    .zip(WEB_PROFILE_DEFAULT_BUNDLES.iter())
                    .all(|(v, name)| v.as_str() == Some(name))
        })
        .unwrap_or(false);
    if is_untouched_default {
        if let Some(bundles) = manifest
            .pointer_mut("/dsh/profile/bundles")
            .and_then(|v| v.as_array_mut())
        {
            bundles.push(serde_json::Value::String(DSH_MARKET_PACKAGE.to_string()));
            fs::write(
                &manifest_path,
                serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())? + "\n",
            )
            .map_err(|e| format!("写入 profile manifest 失败: {}", e))?;
        }
    }
    Ok(())
}

fn spawn_dsh_web(app: &AppHandle) -> Result<DshProcess, String> {
    // Preinstall the plugin market into the web profile. Optional: a failure
    // here must never block DSH from starting.
    if let Err(e) = seed_dsh_market(app) {
        log_error(app, &format!("预装 dsh-market 失败: {}", e));
    }

    let runtime = runtime_dir(app)?;
    let node = runtime.join(node_bin_name());
    let script = runtime.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    if !node.exists() || !script.exists() {
        return Err("运行环境不完整，请重启应用以重新初始化".to_string());
    }

    status(app, "正在启动 DSH 服务…");

    // DSH resolves its default workspace and config against process.cwd().
    // A GUI app launched from Explorer/Finder can inherit a read-only cwd
    // (C:\Windows\System32, install dir, /), which breaks state persistence
    // and can kill the process before it prints the ready line. Anchor it to
    // the user's home directory instead.
    let work_dir = app.path().home_dir().map_err(|e| e.to_string())?;

    let mut cmd = Command::new(&node);
    // --no-open: dsh web otherwise hands the URL to the system's default
    // browser; the desktop app navigates its own window instead.
    cmd.arg(script)
        .arg("web")
        .arg("--no-open")
        .arg("--port")
        .arg("0")
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // node.exe is a console-subsystem executable; without CREATE_NO_WINDOW,
    // Windows allocates a visible console window for it. stdout/stderr are
    // piped, so the console is pure noise.
    #[cfg(windows)]
    hide_console(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| {
        format!("启动 DSH 失败 ({}): {}{}", node.display(), e, access_denied_hint(&e))
    })?;

    // Record the PID so a later launch can clean up if this child outlives
    // the app (window closed to tray, crash, forced update).
    let pid_path = runtime.join("dsh.pid");
    let _ = fs::write(&pid_path, child.id().to_string());

    // Drain stderr so the child never blocks on a full pipe, and log every
    // line — DSH reports warnings and errors on stderr.
    if let Some(stderr) = child.stderr.take() {
        let log_handle = app.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    log_error(&log_handle, &format!("DSH stderr: {}", trimmed));
                }
            }
        });
    }

    let url = Arc::new(Mutex::new(None::<String>));
    let url_clone = url.clone();
    let app_handle = app.clone();

    let stdout = child.stdout.take().ok_or("stdout unavailable")?;
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(pos) = line.find("dsh web: ") {
                // Line shape: `dsh web: <url>` or `dsh web: <url> (LAN: …)` —
                // take the first whitespace-separated token as the URL.
                if let Some(found) = line[pos + 9..].split_whitespace().next() {
                    *url_clone.lock().unwrap() = Some(found.to_string());
                    // Navigate the Tauri window to the official DSH web UI.
                    if let Some(window) = app_handle.get_webview_window("main") {
                        if let Ok(parsed) = tauri::Url::parse(found) {
                            let _ = window.navigate(parsed);
                        }
                    }
                    // Notify the frontend shell as well.
                    let _ = app_handle.emit("dsh-ready", found);
                    status(&app_handle, "DSH 服务已就绪");
                }
            }
        }
        // stdout closed before the ready line means the child died early.
        if url_clone.lock().unwrap().is_none() {
            log_error(&app_handle, "DSH 进程在未输出就绪信号前退出");
            let _ = app_handle.emit("dsh-error", "DSH 进程异常退出，详见 ~/deepseek-work-preview/deepseek-work.log");
        }
    });

    Ok(DshProcess { child, url, pid_path })
}

/// Webview script that hides non-whitelisted plugins from the dsh-market
/// settings section. The market UI loads its catalog via
/// `fetch("/dsh-market/registry")`; wrapping `window.fetch` lets us filter
/// `data.registry.plugins` purely at the presentation layer — the market
/// package and the DSH server (including its install endpoint) stay
/// untouched.
///
/// The allowlist itself is NOT bundled: it is fetched once per page load from
/// a designated GitHub repo. That repo's plugins.json is an audit index —
/// `plugins[]` entries keyed by `name` ("owner/repo") with a `verdict` of
/// whitelist / greylist / blacklist / pending; only `whitelist` entries may be
/// shown. Market registry entries join on `owner + "/" + name`. The set starts
/// empty, and an empty set hides everything — so while the config is
/// unreachable (offline, malformed JSON) the market shows nothing.
///
/// The canonical source is the GitHub raw URL, but that host is unreachable
/// from some networks (mainland China), so a jsDelivr mirror of the same repo
/// is tried as a fallback. See [`MARKET_ALLOWLIST_URLS`].
const MARKET_ALLOWLIST_URLS: [&str; 2] = [
    "https://raw.githubusercontent.com/hotpot-labs/awesome-dsh-industry-plugins/main/plugins.json",
    "https://cdn.jsdelivr.net/gh/hotpot-labs/awesome-dsh-industry-plugins@main/plugins.json",
];

/// Per-source fetch timeout (ms). A dead source (e.g. a host that blackholes
/// packets instead of failing fast) is aborted after this long so the next
/// source is tried and the market does not stall indefinitely.
const ALLOWLIST_FETCH_TIMEOUT_MS: u64 = 8000;

/// 生成注入到 DSH 市场页面的白名单过滤脚本。
///
/// 脚本包装 `window.fetch`，拦截市场的目录请求 `/dsh-market/registry`，把响应
/// 中的 `data.registry.plugins` 过滤为仅白名单内的插件后返回新的 `Response`。
/// 过滤只作用于表现层，市场包与 DSH 服务端（含安装接口）均不受影响。
///
/// 白名单不内置于安装包：每次页面加载依次尝试 `MARKET_ALLOWLIST_URLS` 中的源，
/// 第一个可达且合法的生效（raw 被墙时回退 jsDelivr 镜像），每个源带超时。该
/// 文件是审计索引，`plugins[]` 每项以 `name`（`owner/repo` 或 `owner/repo#子路径`）
/// 为键、带 `verdict`（whitelist / greylist / blacklist / pending），仅
/// `verdict === 'whitelist'` 且 `name` 非空的条目加入允许集合。
///
/// 匹配规则是**大小写敏感**的精确比对：市场 registry 条目按 `owner + "/" +
/// name` 拼接后与允许集合逐字符比对。允许集合初始为空、空集合隐藏全部插件
/// （fail-closed），因此白名单不可达（离线、JSON 解析失败）时市场展示为空。
fn market_filter_script() -> String {
    let urls_js = MARKET_ALLOWLIST_URLS
        .iter()
        .map(|u| format!("\"{}\"", u))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"(() => {{
  // 幂等保护：同一页面只注入一次，避免重复包装 fetch
  if (window.__dswMarketFilter) return;
  window.__dswMarketFilter = true;
  const ALLOW = new Set();  // 白名单集合，name 为 owner/repo 或 owner/repo#子路径
  window.__dswMarketAllow = ALLOW;
  const orig = window.fetch.bind(window);
  // 把错误转发给 Rust 写日志（注入脚本运行在 DSH 页面，无法直接写文件）
  const logErr = (err) => {{
    try {{
      const msg = err instanceof Error ? (err.message || String(err)) : String(err);
      const g = window.__TAURI__;
      if (g && g.core && typeof g.core.invoke === 'function') return void g.core.invoke('log_frontend_error', {{ msg: msg }});
      const i = window.__TAURI_INTERNALS__;
      if (i && typeof i.invoke === 'function') void i.invoke('log_frontend_error', {{ msg: msg }});
    }} catch (_) {{}}
  }};
  // 依次尝试多个白名单源，第一个可达且合法的生效；每个源带超时，被墙的源不会拖死市场
  const ALLOW_URLS = [{urls}];
  const fetchJson = (u, ms) => {{
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), ms);
    return orig(u, {{ cache: 'no-cache', signal: ctrl.signal }})
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error('HTTP ' + r.status))))
      .finally(() => clearTimeout(timer));
  }};
  let allowReady = null;  // 白名单加载完成的 Promise（成功或全部失败都会 resolve）
  const loadAllowlist = () => {{
    if (allowReady) return allowReady;
    allowReady = (async () => {{
      for (const u of ALLOW_URLS) {{
        try {{
          const cfg = await fetchJson(u, {timeout_ms});
          const list = cfg && Array.isArray(cfg.plugins) ? cfg.plugins : [];
          for (const e of list)
            if (e && e.verdict === 'whitelist' && typeof e.name === 'string' && e.name) ALLOW.add(e.name);
          return;  // 该源加载成功，结束尝试
        }} catch (err) {{ logErr(err); }}  // 该源失败，尝试下一个
      }}
    }})();
    return allowReady;
  }};
  loadAllowlist();  // 页面加载即开始拉取，与市场首次渲染并行
  window.fetch = async (...args) => {{
    const res = await orig(...args);
    try {{
      const input = args[0];
      const url = typeof input === 'string' ? input : (input && input.url) || '';
      // 仅拦截市场目录请求，其余请求原样放行
      if (!url.includes('/dsh-market/registry')) return res;
      const data = await res.clone().json();
      const plugins = data && data.registry && data.registry.plugins;
      if (!Array.isArray(plugins)) return res;
      // 等白名单加载完成再过滤，避免与市场的 registry 请求竞态、误判白名单为空
      try {{ await allowReady; }} catch (_) {{}}
      // 大小写敏感的精确匹配：owner + '/' + name 逐字符命中白名单才展示
      data.registry.plugins = plugins.filter((p) => p && ALLOW.has(p.owner + '/' + p.name));
      return new Response(JSON.stringify(data), {{
        status: res.status,
        statusText: res.statusText,
        headers: {{ 'content-type': 'application/json' }},
      }});
    }} catch (err) {{
      logErr(err);  // 记录到日志文件
      return res;  // 解析失败原样返回，不阻断市场
    }}
  }};
}})();"#,
        urls = urls_js,
        timeout_ms = ALLOWLIST_FETCH_TIMEOUT_MS
    )
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        // Re-inject on every page load: the window first loads the local
        // shell, then navigates to the DSH web UI (a fresh page load).
        .on_page_load(|window, payload| {
            if payload.event() != tauri::webview::PageLoadEvent::Finished {
                return;
            }
            let _ = window.eval(&market_filter_script());
        })
        .invoke_handler(tauri::generate_handler![
            get_dsh_status,
            start_dsh_service,
            stop_dsh_service,
            restart_dsh_service,
            app_version,
            log_frontend_error
        ])
        .setup(|app| {
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
            let restart_i = MenuItem::with_id(app, "restart", "重启 DSH", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &show_i,
                    &restart_i,
                    &PredefinedMenuItem::separator(app)?,
                    &quit_i,
                ],
            )?;

            let tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        // Synchronously kill the DSH child before exiting:
                        // `app.exit` skips Drop, so without this node.exe is
                        // orphaned and locks the runtime dir on next launch.
                        if let Ok(mut guard) = app.state::<AppState>().dsh.lock() {
                            guard.take();
                        }
                        app.exit(0);
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "restart" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = handle.emit("dsh-restart-requested", ());
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            // Keep the tray icon alive for the application lifetime.
            let _ = tray;

            // Hide window instead of closing so background tasks keep running.
            if let Some(window) = app.get_webview_window("main") {
                let window_clone = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_clone.hide();
                    }
                });
            }

            // Boot sequence: extract runtime on first launch → start DSH.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                status(&handle, "正在初始化…");

                if let Err(err) = ensure_runtime(&handle, &handle.state()) {
                    status(&handle, &err);
                    log_error(&handle, &format!("运行环境初始化失败: {}", err));
                    let _ = handle.emit("dsh-error", err);
                    return;
                }

                let state: State<AppState> = handle.state();
                let mut guard = state.dsh.lock().unwrap();
                if guard.is_none() {
                    match spawn_dsh_web(&handle) {
                        Ok(proc) => {
                            *guard = Some(proc);
                        }
                        Err(err) => {
                            status(&handle, &err);
                            log_error(&handle, &format!("启动 DSH 失败: {}", err));
                            let _ = handle.emit("dsh-error", err);
                        }
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

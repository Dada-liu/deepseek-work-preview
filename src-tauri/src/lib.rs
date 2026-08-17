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
}

impl Drop for DshProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
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
            fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
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

fn spawn_dsh_web(app: &AppHandle) -> Result<DshProcess, String> {
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

    let mut cmd = Command::new(node);
    cmd.arg(script)
        .arg("web")
        .arg("--port")
        .arg("0")
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // node.exe is a console-subsystem executable; without CREATE_NO_WINDOW,
    // Windows allocates a visible console window for it. stdout/stderr are
    // piped, so the console is pure noise.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| format!("启动 DSH 失败: {}", e))?;

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

    Ok(DshProcess { child, url })
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_dsh_status,
            start_dsh_service,
            stop_dsh_service,
            restart_dsh_service,
            app_version
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

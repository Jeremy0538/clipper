//! Owns the hidden OBS process: writes its config, starts it, connects, builds the
//! scene, and restarts it if it dies.

pub mod config;
pub mod download;
pub mod setup;
pub mod ws;

use anyhow::{bail, Result};
use config::{ObsPaths, Settings, COLLECTION, PROFILE, WS_PORT};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::process::Command;
use ws::ObsClient;

pub struct ObsManager {
    pub client: ObsClient,
    pub paths: ObsPaths,
    pub data_dir: PathBuf,
    /// OBS saves replays here; the app moves them into the clips folder.
    pub incoming: PathBuf,
    /// Read again on every OBS (re)start; the buffer length can change at runtime.
    pub settings: Mutex<Settings>,
    password: String,
    pid: Mutex<Option<u32>>,
    quitting: AtomicBool,
    /// Set by `restart()`: the next OBS exit is expected.
    restarting: AtomicBool,
}

impl ObsManager {
    pub fn new(data_dir: PathBuf, settings: Settings) -> Result<Arc<Self>> {
        let incoming = data_dir.join("incoming");
        std::fs::create_dir_all(&incoming)?;
        Ok(Arc::new(Self {
            client: ObsClient::new(),
            paths: ObsPaths::resolve(&data_dir),
            password: config::ws_password(&data_dir)?,
            data_dir,
            incoming,
            settings: Mutex::new(settings),
            pid: Mutex::new(None),
            quitting: AtomicBool::new(false),
            restarting: AtomicBool::new(false),
        }))
    }

    /// Supervisor loop; `report` receives short state strings for the UI.
    pub async fn run(self: Arc<Self>, report: impl Fn(String) + Send + Sync + 'static) {
        let mut failures = 0u32;
        while !self.quitting.load(Ordering::SeqCst) {
            let started = Instant::now();
            match self.run_once(&report).await {
                Ok(()) => report("restarting".into()),
                Err(e) => report(format!("error: {e:#}")),
            }
            if self.quitting.load(Ordering::SeqCst) {
                break;
            }
            // Back off if OBS keeps dying right away.
            failures = if started.elapsed() > Duration::from_secs(60) { 0 } else { failures + 1 };
            tokio::time::sleep(Duration::from_secs((3 * failures as u64).clamp(3, 60))).await;
        }
    }

    async fn run_once(&self, report: &impl Fn(String)) -> Result<()> {
        #[cfg(windows)]
        {
            let root = self.data_dir.join("obs");
            if !download::installed(&root) {
                report("downloading OBS 0%".into());
                download::install(&root, |p| report(format!("downloading OBS {p}%"))).await?;
            }
        }
        let settings = self.settings.lock().unwrap().clone();
        config::write_all(&self.paths, &settings, &self.incoming, &self.password)?;
        #[cfg(not(windows))]
        setup::ensure_dev_media(&self.data_dir)?;

        let mut cmd = Command::new(&self.paths.exe);
        cmd.current_dir(&self.paths.cwd)
            .args(&self.paths.extra_args)
            .args([
                "--minimize-to-tray",
                "--multi",
                "--disable-shutdown-check",
                "--disable-updater",
                "--disable-missing-files-check",
                "--profile",
                PROFILE,
                "--collection",
                COLLECTION,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        if let Some(x) = &self.paths.xdg_config_home {
            cmd.env("XDG_CONFIG_HOME", x);
        }
        report("starting".into());
        self.kill_orphans().await;
        let mut child = cmd.spawn()?;
        *self.pid.lock().unwrap() = child.id();
        #[cfg(windows)]
        if let Some(pid) = child.id() {
            tie_to_clipper(pid);
        }

        let t0 = Instant::now();
        loop {
            if self.client.connect(WS_PORT, &self.password).await.is_ok() {
                break;
            }
            if let Some(st) = child.try_wait()? {
                bail!("OBS exited while starting ({st})");
            }
            if t0.elapsed() > Duration::from_secs(45) {
                let _ = child.start_kill();
                bail!("could not connect to OBS");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // The websocket opens before OBS has finished loading ("not ready",
        // code 207): keep trying. If it never works, restart OBS rather than
        // leaving a half-set-up one running.
        let mut specs = setup::specs(&self.data_dir, &settings.audio);
        specs.extend(setup::webcam_spec(&settings.webcam));
        let t0 = Instant::now();
        loop {
            match setup::ensure_scene(&self.client, &specs, settings.width, settings.height).await {
                Ok(()) => {
                    let _ = setup::set_track_names(&self.client, &settings.audio).await;
                    let _ = setup::place_webcam(&self.client, config::SCENE, settings.width, settings.height, &settings.webcam).await;
                    // Always come up in game mode (OBS may remember the Screen scene).
                    let _ = self.client.call("SetCurrentProgramScene", serde_json::json!({ "sceneName": config::SCENE })).await;
                    break;
                }
                Err(e) if t0.elapsed() < Duration::from_secs(60) => {
                    crate::clog!("scene setup not done yet: {e:#}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if let Some(st) = child.try_wait()? {
                        bail!("OBS exited while starting ({st})");
                    }
                    // The socket may have belonged to an OBS that was still shutting down.
                    if !self.client.is_connected().await {
                        let _ = self.client.connect(WS_PORT, &self.password).await;
                    }
                }
                Err(e) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    bail!("scene setup failed: {e:#}");
                }
            }
        }
        report("ready".into());

        let status = child.wait().await?;
        *self.pid.lock().unwrap() = None;
        if self.restarting.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        if !self.quitting.load(Ordering::SeqCst) {
            bail!("OBS exited ({status})");
        }
        Ok(())
    }

    /// Re-apply the audio part of the scene (after a settings change).
    pub async fn apply_audio(&self) -> Result<()> {
        let s = self.settings.lock().unwrap().clone();
        let mut specs = setup::specs(&self.data_dir, &s.audio);
        specs.extend(setup::webcam_spec(&s.webcam));
        setup::ensure_scene(&self.client, &specs, s.width, s.height).await?;
        setup::place_webcam(&self.client, config::SCENE, s.width, s.height, &s.webcam).await?;
        setup::set_track_names(&self.client, &s.audio).await
    }

    /// An OBS left over from an earlier Clipper (crash, killed in Task Manager)
    /// would fight the new one for the websocket port and the game-capture hook
    /// (black clips). Only OBS processes started from Clipper's own copy are hit.
    async fn kill_orphans(&self) {
        if !cfg!(windows) {
            return;
        }
        let exe = self.paths.exe.to_string_lossy().to_lowercase();
        let killed = tokio::task::spawn_blocking(move || {
            let mut sys = sysinfo::System::new();
            sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::All,
                true,
                sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
            );
            let mut n = 0;
            for p in sys.processes().values() {
                if p.exe().is_some_and(|e| e.to_string_lossy().to_lowercase() == exe) && p.kill() {
                    n += 1;
                }
            }
            n
        })
        .await
        .unwrap_or(0);
        if killed > 0 {
            crate::clog!("closed {killed} leftover OBS process(es)");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// The GPU encoder could not start: switch to x264 once and restart OBS.
    /// Returns false if that already happened.
    pub fn fall_back_to_cpu(&self) -> bool {
        {
            let mut s = self.settings.lock().unwrap();
            if s.force_cpu {
                return false;
            }
            s.force_cpu = true;
        }
        crate::clog!("GPU encoder failed: switching to CPU encoding (x264)");
        self.restart();
        true
    }

    /// Restart OBS so it picks up new encoder settings (only while it's idle).
    pub fn restart(&self) {
        let Some(pid) = *self.pid.lock().unwrap() else { return };
        self.restarting.store(true, Ordering::SeqCst);
        let pid = pid.to_string();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill").args(["/F", "/PID", &pid]).creation_flags(0x0800_0000).status();
        }
        #[cfg(not(windows))]
        let _ = std::process::Command::new("kill").args(["-TERM", &pid]).status();
    }

    /// Stop OBS for good (app is quitting). Gentle first, forced after a few seconds.
    pub fn shutdown(&self) {
        self.quitting.store(true, Ordering::SeqCst);
        let Some(pid) = self.pid.lock().unwrap().take() else { return };
        let pid = pid.to_string();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const NO_WINDOW: u32 = 0x0800_0000;
            let _ = std::process::Command::new("taskkill").args(["/PID", &pid])
                .creation_flags(NO_WINDOW).status();
            std::thread::sleep(Duration::from_secs(3));
            let _ = std::process::Command::new("taskkill").args(["/F", "/PID", &pid])
                .creation_flags(NO_WINDOW).status();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("kill").args(["-TERM", &pid]).status();
            for _ in 0..30 {
                std::thread::sleep(Duration::from_millis(100));
                let alive = std::process::Command::new("kill").args(["-0", &pid])
                    .stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
                if !alive {
                    return;
                }
            }
            let _ = std::process::Command::new("kill").args(["-KILL", &pid]).status();
        }
    }
}

/// Put OBS in a job object that is killed when Clipper's last handle closes:
/// if Clipper crashes or is ended in Task Manager, Windows closes OBS too.
#[cfg(windows)]
fn tie_to_clipper(pid: u32) {
    use std::sync::OnceLock;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::{
                JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                },
                Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
            },
        },
    };
    // One job for the app's lifetime; its handle is deliberately never closed.
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(job.0 as isize)
    });
    let Some(job) = *job else {
        crate::clog!("ERROR could not create the OBS job object");
        return;
    };
    unsafe {
        match OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) {
            Ok(h) => {
                if let Err(e) = AssignProcessToJobObject(HANDLE(job as *mut _), h) {
                    crate::clog!("ERROR could not tie OBS to Clipper: {e}");
                }
                let _ = CloseHandle(h);
            }
            Err(e) => crate::clog!("ERROR could not open OBS process: {e}"),
        }
    }
}

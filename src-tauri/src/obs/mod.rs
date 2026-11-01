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
    pub settings: Settings,
    password: String,
    pid: Mutex<Option<u32>>,
    quitting: AtomicBool,
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
            settings,
            pid: Mutex::new(None),
            quitting: AtomicBool::new(false),
        }))
    }

    /// Supervisor loop; `report` receives short state strings for the UI.
    pub async fn run(self: Arc<Self>, report: impl Fn(String) + Send + Sync + 'static) {
        let mut failures = 0u32;
        while !self.quitting.load(Ordering::SeqCst) {
            let started = Instant::now();
            match self.run_once(&report).await {
                Ok(()) => report("stopped".into()),
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
        config::write_all(&self.paths, &self.settings, &self.incoming, &self.password)?;
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
        let mut child = cmd.spawn()?;
        *self.pid.lock().unwrap() = child.id();

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

        let specs = setup::specs(&self.data_dir);
        if let Err(e) =
            setup::ensure_scene(&self.client, &specs, self.settings.width, self.settings.height).await
        {
            report(format!("error: scene setup: {e:#}"));
        } else {
            report("ready".into());
        }

        let status = child.wait().await?;
        *self.pid.lock().unwrap() = None;
        if !self.quitting.load(Ordering::SeqCst) {
            bail!("OBS exited ({status})");
        }
        Ok(())
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

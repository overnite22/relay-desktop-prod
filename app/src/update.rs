//! « Rechercher des mises à jour » : checks the published releases, downloads
//! and verifies the MSI (SHA-256, plus publisher when Relay is signed), then
//! hands over to msiexec and restarts Relay once installed.

use crate::{with, AppWindow, AGENT};
use relay_desktop_core::update::{Release, Updater, DEFAULT_API, DEFAULT_REPO};
use slint::ComponentHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static PENDING: Mutex<Option<Release>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
const DAY: u64 = 24 * 3600;

fn updater() -> Updater {
    // Overridable for tests against a local fake API (loopback only, see core::update).
    let api = std::env::var("RELAY_UPDATE_API").unwrap_or_else(|_| DEFAULT_API.into());
    let repo = std::env::var("RELAY_UPDATE_REPO").unwrap_or_else(|_| DEFAULT_REPO.into());
    Updater::new(&api, &repo, AGENT)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn on_ui(weak: slint::Weak<AppWindow>, f: impl FnOnce(&AppWindow) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            f(&ui)
        }
    });
}

/// Manual check from the ribbon / menu.
pub fn check(ui: &AppWindow) {
    ui.set_upd_state("checking".into());
    ui.set_upd_latest("".into());
    ui.set_upd_notes("".into());
    ui.set_dialog_error("".into());
    ui.set_dialog("update".into());
    run(ui, true);
}

/// Silent check at startup, at most once a day, unless disabled.
pub fn check_on_startup(ui: &AppWindow) {
    let due = with(|s| {
        let due = s.settings.auto_update && now().saturating_sub(s.settings.last_update_check) >= DAY;
        if due {
            s.settings.last_update_check = now();
            let _ = s.settings.save();
        }
        due
    });
    if due {
        run(ui, false);
    }
}

fn run(ui: &AppWindow, manual: bool) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = updater().available(env!("CARGO_PKG_VERSION"));
        on_ui(weak, move |ui| {
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            match result {
                Ok(Some(r)) => {
                    // A silent check only interrupts when nothing else is open.
                    if !manual && !ui.get_dialog().is_empty() {
                        ui.set_status(format!("Relay {} est disponible : Aide → Rechercher des mises à jour", r.version).into());
                        *PENDING.lock().unwrap() = Some(r);
                        return;
                    }
                    ui.set_upd_latest(r.version.to_string().into());
                    ui.set_upd_notes(r.notes.trim().to_string().into());
                    ui.set_upd_state("available".into());
                    ui.set_dialog("update".into());
                    *PENDING.lock().unwrap() = Some(r);
                }
                Ok(None) if manual => ui.set_upd_state("uptodate".into()),
                Err(e) if manual => {
                    ui.set_upd_message(e.to_string().into());
                    ui.set_upd_state("error".into());
                }
                _ => {}
            }
        });
    });
}

pub fn cancel() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// « Mettre à jour » : download, verify, start the installer, quit.
pub fn install(ui: &AppWindow) {
    let Some(release) = PENDING.lock().unwrap().clone() else { return };
    if ui.get_upd_state() != "available" {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    ui.set_upd_state("downloading".into());
    ui.set_upd_message(format!("Téléchargement de Relay {}…", release.version).into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let dir = std::env::temp_dir()
            .join("Relay")
            .join("updates")
            .join(format!("{}-{generation}", std::process::id()));
        let progress_ui = weak.clone();
        let last = Mutex::new(0u64);
        let result = updater()
            .download_cancellable(
                &release,
                &dir,
                |done, total| {
                    let pct = (done * 100).checked_div(total).unwrap_or(0);
                    let mut l = last.lock().unwrap();
                    if pct != *l {
                        *l = pct;
                        let text = format!("Téléchargement de Relay {} : {pct} %", release.version);
                        on_ui(progress_ui.clone(), move |ui| {
                            if GENERATION.load(Ordering::SeqCst) == generation {
                                ui.set_upd_message(text.into());
                            }
                        });
                    }
                },
                || GENERATION.load(Ordering::SeqCst) != generation,
            )
            .map_err(|e| e.to_string())
            .and_then(|msi| platform::verify_publisher(&msi).map(|_| msi));
        on_ui(weak, move |ui| {
            // Cancellation and launch both happen on the UI thread, so closing
            // the dialog cannot race with a worker starting the installer.
            if GENERATION.load(Ordering::SeqCst) != generation {
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            let launched = result.and_then(|msi| platform::launch_installer(&msi));
            match launched {
                Ok(()) => {
                    ui.set_upd_message("Installation en cours, Relay va redémarrer…".into());
                    let _ = slint::quit_event_loop();
                }
                Err(e) => {
                    ui.set_upd_message(e.into());
                    ui.set_upd_state("error".into());
                }
            }
        });
    });
}

#[allow(dead_code)]
fn quoted_path_ok(p: &Path) -> bool {
    let s = p.to_string_lossy();
    !s.is_empty() && !s.chars().any(|c| c == '"' || c == '%' || c.is_control())
}

#[cfg(windows)]
mod platform {
    use super::{quoted_path_ok, Path, PathBuf};
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    fn system32(exe: &str) -> PathBuf {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| r"C:\Windows".into())
            .join("System32")
            .join(exe)
    }

    /// Authenticode status and signer of a file, via PowerShell (paths passed
    /// through the environment, never interpolated in the script).
    fn signature(path: &Path) -> Result<(String, String), String> {
        let out = Command::new(system32(r"WindowsPowerShell\v1.0\powershell.exe"))
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$s = Get-AuthenticodeSignature -LiteralPath $env:RELAY_SIG_PATH; \"$($s.Status)|$($s.SignerCertificate.Subject)\"",
            ])
            .env("RELAY_SIG_PATH", path)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("vérification de signature impossible : {e}"))?;
        if !out.status.success() {
            return Err("la vérification de signature a échoué".into());
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let (status, subject) = text.split_once('|').unwrap_or((text.as_str(), ""));
        Ok((status.to_string(), subject.to_string()))
    }

    /// Extension point for signed builds: when the running Relay is signed,
    /// the update must carry a valid signature from the same publisher.
    /// Unsigned builds rely on the SHA-256 check alone.
    pub fn verify_publisher(msi: &Path) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let (status, subject) = signature(&exe)?;
        if status == "NotSigned" {
            return Ok(());
        }
        if status != "Valid" || subject.is_empty() {
            return Err("la signature du programme ne peut pas être vérifiée : mise à jour annulée".into());
        }
        let (msi_status, msi_subject) = signature(msi)?;
        if msi_status != "Valid" || msi_subject != subject {
            let _ = std::fs::remove_file(msi);
            return Err(format!(
                "le paquet n'est pas signé par l'éditeur de Relay ({msi_status}) : mise à jour annulée"
            ));
        }
        Ok(())
    }

    /// Runs msiexec through a small script that restarts Relay when the
    /// installation succeeds (0, or 3010 = reboot required later).
    pub fn launch_installer(msi: &Path) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let script = msi.with_extension("cmd");
        if ![msi, exe.as_path(), script.as_path()].iter().all(|p| quoted_path_ok(p)) {
            return Err("chemin d'installation non pris en charge".into());
        }
        let body = format!(
            "@echo off\r\n\"{}\" /i \"%~1\" /passive /norestart\r\nset rc=%ERRORLEVEL%\r\nif \"%rc%\"==\"0\" start \"\" \"%~2\"\r\nif \"%rc%\"==\"3010\" start \"\" \"%~2\"\r\n",
            system32("msiexec.exe").display()
        );
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        Command::new(system32("cmd.exe"))
            .raw_arg(format!(
                "/d /c \"\"{}\" \"{}\" \"{}\"\"",
                script.display(),
                msi.display(),
                exe.display()
            ))
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
            .spawn()
            .map_err(|e| format!("lancement de l'installeur impossible : {e}"))?;
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Path;
    pub fn verify_publisher(_: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn launch_installer(_: &Path) -> Result<(), String> {
        Err("installation disponible uniquement sous Windows".into())
    }
}

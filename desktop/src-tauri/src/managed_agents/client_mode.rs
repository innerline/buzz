//! Per-install client-only mode: a Buzz install that reads the relay but
//! never runs agent engines locally.
//!
//! Rationale (2026-10-30, laptop incident): Buzz Desktop auto-restores every
//! saved agent's engine on whatever machine the app opens on, with no
//! per-install way to say "this machine is a client only". A laptop that
//! opens the app once grows its own engine fleet — exactly what the owner's
//! architecture forbids: the laptop is a client, it never runs anything.
//!
//! Two switches, checked in order:
//! 1. `BUZZ_CLIENT_ONLY` env var — truthy values (`1`/`true`/`yes`/`on`)
//!    force ON, falsy values (`0`/`false`/`no`/`off`) force OFF. The explicit
//!    OFF beats the marker file so one launch can be un-gated for diagnosis
//!    without touching the install.
//! 2. Marker file `<app_data_dir>/client-only` — present means ON.
//!
//! Env unset + no marker = a normal full install (default OFF), so the
//! engine host keeps spawning exactly as before. Nothing gates on frontend
//! state: the check runs at every local engine spawn and at launch restore,
//! so a client-only install cannot regrow engines even when a stale record
//! says `start_on_app_launch`.
//!
//! Non-goals: this gate covers Buzz-owned work only - managed-agent engine
//! spawns, the mesh (Share Compute) coordinator, Share Compute restore, and
//! every local mesh-runtime start: the `mesh_start_node` command (serving
//! or client mode), `ensure_relay_mesh_for_record` (serve restore and the
//! client fallback), and `restore_mesh_sharing`. It does not touch other
//! applications or the engines they spawn (e.g. Obsidian Copilot), and it
//! does not gate one-shot, time-boxed CLI probes (`invoke_provider` and
//! discovery), which start no persistent engine.
//! The desktop app never starts a relay process - no relay binary ships in
//! `externalBin`; the relay is a standalone service - so a client-only
//! install binds no relay port by construction.

use tauri::{AppHandle, Manager, Runtime};

/// Marker file that turns a Buzz install into a client-only install.
pub(crate) const CLIENT_ONLY_MARKER: &str = "client-only";

/// Env-var override for [`client_only_enabled`]. Values are
/// case-insensitive; an unparseable value is ignored (marker file decides).
pub(crate) const CLIENT_ONLY_ENV_VAR: &str = "BUZZ_CLIENT_ONLY";

/// Stable prefix for spawn refusals on a client-only install, so tests and
/// humans reading a start error can tell this refusal apart from a key
/// refusal ("has no private key available") or an admission refusal.
pub(crate) const CLIENT_ONLY_ERROR_PREFIX: &str =
    "client-only install: this Buzz app is configured as a client and does not run agent engines";

fn env_override() -> Option<bool> {
    let raw = std::env::var(CLIENT_ONLY_ENV_VAR).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn marker_path<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join(CLIENT_ONLY_MARKER))
}

/// True when this install must never run agent engines: it connects to a
/// relay elsewhere and displays conversations, but spawns nothing locally.
pub(crate) fn client_only_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    if let Some(forced) = env_override() {
        return forced;
    }
    marker_path(app).is_some_and(|path| path.exists())
}

/// `Some(refusal)` when a local engine spawn must be refused, mirroring
/// [`super::spawn_key_refusal`]'s fail-closed contract: callers fail closed,
/// never partially start.
pub(crate) fn client_only_refusal<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    if !client_only_enabled(app) {
        return None;
    }
    let marker = marker_path(app)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<app data dir unavailable>".to_string());
    Some(format!(
        "{CLIENT_ONLY_ERROR_PREFIX} (marker: {marker}; set {CLIENT_ONLY_ENV_VAR}=0 to override \
         for one launch)"
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        client_only_enabled, client_only_refusal, CLIENT_ONLY_ENV_VAR, CLIENT_ONLY_MARKER,
    };
    use crate::managed_agents::lock_env_mutex;
    use tauri::Manager;

    /// Mock app whose app data dir is a fresh temp dir (same trick as
    /// `admission_test_support`: an absolute identifier replaces the app
    /// data base dir), so marker-file state never leaks between tests.
    /// The temp dir is returned alongside: dropping it deletes the dir the
    /// app handle still points at, so tests drop it last.
    fn app_with_temp_data_dir() -> (tauri::App<tauri::test::MockRuntime>, tempfile::TempDir) {
        let data = tempfile::tempdir().unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = data.path().to_str().unwrap().to_owned();
        let app = tauri::test::mock_builder().build(context).unwrap();
        (app, data)
    }

    /// Removes the env var on drop, so a failing assert cannot leak state
    /// into tests running on other threads.
    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    fn write_marker(app: &tauri::App<tauri::test::MockRuntime>) {
        let dir = app.path().app_data_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CLIENT_ONLY_MARKER), b"").unwrap();
    }

    #[test]
    fn no_marker_and_no_env_is_a_full_install() {
        let (app, _data) = app_with_temp_data_dir();
        let _env = lock_env_mutex();
        std::env::remove_var(CLIENT_ONLY_ENV_VAR);
        assert!(!client_only_enabled(app.handle()));
        assert_eq!(client_only_refusal(app.handle()), None);
    }

    #[test]
    fn marker_file_enables_client_only() {
        let (app, _data) = app_with_temp_data_dir();
        write_marker(&app);
        assert!(client_only_enabled(app.handle()));
        let refusal = client_only_refusal(app.handle()).unwrap();
        assert!(
            refusal.starts_with(super::CLIENT_ONLY_ERROR_PREFIX),
            "{refusal}"
        );
    }

    #[test]
    fn env_var_forces_on_without_a_marker() {
        let (app, _data) = app_with_temp_data_dir();
        let _env = lock_env_mutex();
        let _guard = EnvGuard(CLIENT_ONLY_ENV_VAR);
        std::env::set_var(CLIENT_ONLY_ENV_VAR, "1");
        assert!(client_only_enabled(app.handle()));
        std::env::set_var(CLIENT_ONLY_ENV_VAR, "true");
        assert!(client_only_enabled(app.handle()));
    }

    #[test]
    fn env_var_off_beats_the_marker_file() {
        let (app, _data) = app_with_temp_data_dir();
        write_marker(&app);
        let _env = lock_env_mutex();
        let _guard = EnvGuard(CLIENT_ONLY_ENV_VAR);
        std::env::set_var(CLIENT_ONLY_ENV_VAR, "0");
        assert!(!client_only_enabled(app.handle()));
        assert_eq!(client_only_refusal(app.handle()), None);
    }

    #[test]
    fn unparseable_env_value_is_ignored() {
        let (app, _data) = app_with_temp_data_dir();
        let _env = lock_env_mutex();
        let _guard = EnvGuard(CLIENT_ONLY_ENV_VAR);
        std::env::set_var(CLIENT_ONLY_ENV_VAR, "maybe");
        assert!(!client_only_enabled(app.handle()));
    }

    #[test]
    fn unparseable_env_value_leaves_the_marker_in_charge() {
        // Marker present + unparseable env: the env value is ignored, so the
        // marker decides and the install is client-only.
        let (app, _data) = app_with_temp_data_dir();
        write_marker(&app);
        let _env = lock_env_mutex();
        let _guard = EnvGuard(CLIENT_ONLY_ENV_VAR);
        std::env::set_var(CLIENT_ONLY_ENV_VAR, "maybe");
        assert!(client_only_enabled(app.handle()));
        let refusal = client_only_refusal(app.handle()).unwrap();
        assert!(
            refusal.starts_with(super::CLIENT_ONLY_ERROR_PREFIX),
            "{refusal}"
        );
    }
}

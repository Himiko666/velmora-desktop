use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tauri::{Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_store::StoreExt;
use tauri_plugin_updater::UpdaterExt;

const API_BASE: &str = "https://velmora.cc/api/mobile";
const GAME_URL: &str = "https://velmora.cc";
const SSO_TIMEOUT_SECS: u64 = 10;
const STORE_FILE: &str = "velmora.json";
const TOKEN_KEY: &str = "auth_token";
const DEVICE_NAME: &str = "Velmora Desktop";

// ---------------- Erreurs ----------------

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("réseau : {0}")]
    Network(#[from] reqwest::Error),
    #[error("non authentifié")]
    Unauthenticated,
    #[error("identifiants invalides")]
    InvalidCredentials,
    #[error("réponse inattendue du serveur : {0}")]
    Unexpected(String),
    #[error("erreur interne : {0}")]
    Internal(String),
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

type AppResult<T> = Result<T, AppError>;

// ---------------- État global ----------------

#[derive(Default)]
struct AuthState {
    token: Mutex<Option<String>>,
}

// ---------------- Payloads ----------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserPayload {
    #[serde(flatten)]
    pub data: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug)]
struct LoginRequest<'a> {
    email: &'a str,
    password: &'a str,
    device_name: &'a str,
}

#[derive(Serialize, Deserialize, Debug)]
struct LoginResponse {
    token: String,
    user: serde_json::Value,
}

// ---------------- Helpers ----------------

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("VelmoraDesktop/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("client HTTP")
}

fn read_token(app: &tauri::AppHandle, state: &State<AuthState>) -> Option<String> {
    if let Some(tok) = state.token.lock().ok().and_then(|g| g.clone()) {
        return Some(tok);
    }
    let store = app.store(STORE_FILE).ok()?;
    let value = store.get(TOKEN_KEY)?;
    value.as_str().map(|s| s.to_string())
}

fn save_token(app: &tauri::AppHandle, state: &State<AuthState>, token: &str) -> AppResult<()> {
    if let Ok(mut g) = state.token.lock() {
        *g = Some(token.to_string());
    }
    let store = app
        .store(STORE_FILE)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    store.set(TOKEN_KEY, serde_json::Value::String(token.to_string()));
    store
        .save()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

fn clear_token(app: &tauri::AppHandle, state: &State<AuthState>) {
    if let Ok(mut g) = state.token.lock() {
        *g = None;
    }
    if let Ok(store) = app.store(STORE_FILE) {
        store.delete(TOKEN_KEY);
        let _ = store.save();
    }
}

// ---------------- Commandes Tauri ----------------

#[tauri::command]
async fn login(
    app: tauri::AppHandle,
    state: State<'_, AuthState>,
    email: String,
    password: String,
) -> AppResult<serde_json::Value> {
    let client = http_client();
    let body = LoginRequest {
        email: &email,
        password: &password,
        device_name: DEVICE_NAME,
    };

    let resp = client
        .post(format!("{}/login", API_BASE))
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
        || status == reqwest::StatusCode::UNAUTHORIZED
    {
        return Err(AppError::InvalidCredentials);
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(AppError::Unexpected(format!("HTTP {} — {}", status, txt)));
    }

    let parsed: LoginResponse = resp.json().await.map_err(AppError::from)?;
    save_token(&app, &state, &parsed.token)?;
    Ok(parsed.user)
}

#[tauri::command]
async fn logout(app: tauri::AppHandle, state: State<'_, AuthState>) -> AppResult<()> {
    let token = read_token(&app, &state);
    if let Some(t) = token {
        let client = http_client();
        let _ = client
            .post(format!("{}/logout", API_BASE))
            .bearer_auth(&t)
            .header("Accept", "application/json")
            .send()
            .await;
    }
    clear_token(&app, &state);
    Ok(())
}

#[tauri::command]
async fn me(app: tauri::AppHandle, state: State<'_, AuthState>) -> AppResult<serde_json::Value> {
    let token = read_token(&app, &state).ok_or(AppError::Unauthenticated)?;
    let client = http_client();
    let resp = client
        .get(format!("{}/me", API_BASE))
        .bearer_auth(&token)
        .header("Accept", "application/json")
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        clear_token(&app, &state);
        return Err(AppError::Unauthenticated);
    }
    if !resp.status().is_success() {
        return Err(AppError::Unexpected(format!("HTTP {}", resp.status())));
    }
    let json: serde_json::Value = resp.json().await?;
    Ok(json)
}

#[tauri::command]
async fn dashboard(
    app: tauri::AppHandle,
    state: State<'_, AuthState>,
) -> AppResult<serde_json::Value> {
    let token = read_token(&app, &state).ok_or(AppError::Unauthenticated)?;
    let client = http_client();
    let resp = client
        .get(format!("{}/dashboard", API_BASE))
        .bearer_auth(&token)
        .header("Accept", "application/json")
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        clear_token(&app, &state);
        return Err(AppError::Unauthenticated);
    }
    if !resp.status().is_success() {
        return Err(AppError::Unexpected(format!("HTTP {}", resp.status())));
    }
    let json: serde_json::Value = resp.json().await?;
    Ok(json)
}

// ---------------- Pré-cache d'assets sur disque ----------------

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AssetEntry {
    key: String,
    url: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    sha: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct AssetManifest {
    #[serde(default)]
    version: String,
    #[serde(default)]
    assets: Vec<AssetEntry>,
    #[serde(default)]
    count: u32,
}

#[derive(Serialize, Clone)]
struct SetupProgress {
    step: String,
    label: String,
    current: u32,
    total: u32,
}

const SETUP_DONE_KEY: &str = "setup_completed";
const CACHED_MANIFEST_VERSION_KEY: &str = "cached_manifest_version";
const ASSET_PARALLELISM: usize = 8;

#[tauri::command]
fn is_first_run(app: tauri::AppHandle) -> bool {
    let Ok(store) = app.store(SETTINGS_FILE) else {
        return true;
    };
    !matches!(store.get(SETUP_DONE_KEY), Some(serde_json::Value::Bool(true)))
}

/// Dossier où on persiste les assets pré-cachés : `app_cache_dir/assets/`.
/// Créé à la volée si absent. Utilisé par le pré-cache premier lancement
/// et par le re-check background après login.
fn assets_dir(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    let base = app
        .path()
        .app_cache_dir()
        .map_err(|e| AppError::Internal(format!("app_cache_dir : {e}")))?;
    let dir = base.join("assets");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::Internal(format!("mkdir assets : {e}")))?;
    Ok(dir)
}

/// Récupère et désérialise le manifeste live depuis le backend. Renvoie
/// un manifeste vide si l'appel échoue — on ne veut pas bloquer le launcher
/// pour un asset-manifest momentanément indisponible.
async fn fetch_manifest(client: &reqwest::Client) -> AssetManifest {
    let empty = AssetManifest {
        version: String::new(),
        assets: vec![],
        count: 0,
    };
    let Ok(resp) = client.get(format!("{}/asset-manifest", API_BASE)).send().await else {
        return empty;
    };
    if !resp.status().is_success() {
        return empty;
    }
    resp.json::<AssetManifest>().await.unwrap_or(empty)
}

/// Télécharge UN asset à `dir/<key>` si nécessaire. Skip si le fichier
/// existe avec la bonne taille (vérif rapide, évite un sha256 par lancement).
/// Tout échec est avalé : un asset KO ne doit pas faire planter le batch.
async fn ensure_asset_cached(
    client: &reqwest::Client,
    asset: &AssetEntry,
    dir: &std::path::Path,
) -> bool {
    let target = dir.join(&asset.key);

    if let Ok(meta) = std::fs::metadata(&target) {
        if asset.size == 0 || meta.len() == asset.size {
            return true;
        }
    }

    let Ok(resp) = client.get(&asset.url).send().await else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(bytes) = resp.bytes().await else {
        return false;
    };

    // Écriture atomique : on passe par un fichier .part qu'on rename ensuite,
    // pour éviter qu'un crash mid-download laisse un fichier tronqué que le
    // skip-check considérerait comme valide au prochain run.
    let tmp = target.with_extension("part");
    if std::fs::write(&tmp, &bytes).is_err() {
        return false;
    }
    std::fs::rename(&tmp, &target).is_ok()
}

/// Pré-cache tous les assets en parallèle (jusqu'à `ASSET_PARALLELISM`
/// téléchargements simultanés). Émet un event `setup://progress` chaque
/// fois qu'un asset termine. Retourne le manifeste utilisé pour permettre
/// au caller de stocker sa version.
async fn precache_assets(
    app: &tauri::AppHandle,
    client: &reqwest::Client,
    manifest: &AssetManifest,
    emit_progress: bool,
) -> AppResult<()> {
    let dir = assets_dir(app)?;
    let total = manifest.assets.len().max(1) as u32;
    let done = std::sync::Arc::new(AtomicU32::new(0));

    let app_clone = app.clone();
    let dir_clone = dir.clone();
    let done_clone = std::sync::Arc::clone(&done);

    stream::iter(manifest.assets.iter().cloned())
        .map(move |asset| {
            let client = client.clone();
            let dir = dir_clone.clone();
            let app = app_clone.clone();
            let done = std::sync::Arc::clone(&done_clone);
            async move {
                let _ok = ensure_asset_cached(&client, &asset, &dir).await;
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                if emit_progress {
                    let _ = app.emit(
                        "setup://progress",
                        SetupProgress {
                            step: "assets".to_string(),
                            label: format!("Téléchargement : {}", asset.key),
                            current: n,
                            total,
                        },
                    );
                }
            }
        })
        .buffer_unordered(ASSET_PARALLELISM)
        .collect::<Vec<_>>()
        .await;

    Ok(())
}

/// Exécute le pré-cache d'assets au premier lancement. Émet des events
/// `setup://progress` que le frontend écoute pour animer la barre. Marque
/// `setup_completed = true` à la fin.
#[tauri::command]
async fn prepare_first_run(app: tauri::AppHandle) -> AppResult<()> {
    let client = http_client();

    let emit = |step: &str, label: &str, current: u32, total: u32| {
        let _ = app.emit(
            "setup://progress",
            SetupProgress {
                step: step.to_string(),
                label: label.to_string(),
                current,
                total,
            },
        );
    };

    // Étape 1 — Vérifier le royaume
    emit("server", "Vérification du royaume…", 0, 4);
    let _ = client.get(format!("{}/status", API_BASE)).send().await.ok();
    emit("server", "Royaume joignable", 1, 4);

    // Étape 2 — Récupérer le manifest des assets
    emit("manifest", "Récupération du manifeste de ressources…", 1, 4);
    let manifest = fetch_manifest(&client).await;
    emit("manifest", "Manifeste reçu", 2, 4);

    // Étape 3 — Pré-cache parallèle sur disque
    precache_assets(&app, &client, &manifest, true).await?;
    emit("assets", "Ressources prêtes", 3, 4);

    // Étape 4 — Marquer setup_completed + stocker la version du manifest
    emit("config", "Configuration des préférences…", 3, 4);
    let store = app
        .store(SETTINGS_FILE)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    store.set(SETUP_DONE_KEY, serde_json::Value::Bool(true));
    store.set("notifications_enabled", serde_json::Value::Bool(true));
    store.set(
        CACHED_MANIFEST_VERSION_KEY,
        serde_json::Value::String(manifest.version.clone()),
    );
    store
        .save()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    emit("done", "Royaume prêt", 4, 4);

    Ok(())
}

/// Re-vérifie le manifeste après login : si la version a changé, refait
/// le pré-cache silencieusement (sans afficher le setup screen). Appelé
/// par le frontend après `refreshAll`.
#[tauri::command]
async fn recheck_manifest(app: tauri::AppHandle) -> AppResult<bool> {
    let client = http_client();
    let manifest = fetch_manifest(&client).await;
    if manifest.version.is_empty() {
        return Ok(false);
    }

    let store = app
        .store(SETTINGS_FILE)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let cached_version = store
        .get(CACHED_MANIFEST_VERSION_KEY)
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    if cached_version == manifest.version {
        return Ok(false);
    }

    // Version changée → on retélécharge en silence et on persiste la
    // nouvelle version. L'UI n'affiche rien pour ne pas couper le user.
    precache_assets(&app, &client, &manifest, false).await?;
    store.set(
        CACHED_MANIFEST_VERSION_KEY,
        serde_json::Value::String(manifest.version),
    );
    store
        .save()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(true)
}

#[tauri::command]
async fn server_status() -> AppResult<serde_json::Value> {
    let client = http_client();
    let resp = client
        .get(format!("{}/status", API_BASE))
        .header("Accept", "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AppError::Unexpected(format!("HTTP {}", resp.status())));
    }
    Ok(resp.json().await?)
}

#[tauri::command]
async fn launcher_feed(
    app: tauri::AppHandle,
    state: State<'_, AuthState>,
) -> AppResult<serde_json::Value> {
    let token = read_token(&app, &state).ok_or(AppError::Unauthenticated)?;
    let client = http_client();
    let resp = client
        .get(format!("{}/launcher-feed", API_BASE))
        .bearer_auth(&token)
        .header("Accept", "application/json")
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        clear_token(&app, &state);
        return Err(AppError::Unauthenticated);
    }
    if !resp.status().is_success() {
        return Err(AppError::Unexpected(format!("HTTP {}", resp.status())));
    }
    Ok(resp.json().await?)
}

// ---------------- Settings (préférences utilisateur) ----------------

const SETTINGS_FILE: &str = "settings.json";

#[tauri::command]
fn get_setting(app: tauri::AppHandle, key: String) -> AppResult<serde_json::Value> {
    let store = app
        .store(SETTINGS_FILE)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(store.get(&key).unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
fn set_setting(app: tauri::AppHandle, key: String, value: serde_json::Value) -> AppResult<()> {
    let store = app
        .store(SETTINGS_FILE)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    store.set(&key, value);
    store
        .save()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

#[tauri::command]
fn get_autostart(app: tauri::AppHandle) -> AppResult<bool> {
    let manager = app.autolaunch();
    manager
        .is_enabled()
        .map_err(|e| AppError::Internal(e.to_string()))
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> AppResult<()> {
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|e| AppError::Internal(e.to_string()))?;
    } else {
        manager
            .disable()
            .map_err(|e| AppError::Internal(e.to_string()))?;
    }
    Ok(())
}

#[tauri::command]
async fn news(app: tauri::AppHandle, state: State<'_, AuthState>) -> AppResult<serde_json::Value> {
    let token = read_token(&app, &state).ok_or(AppError::Unauthenticated)?;
    let client = http_client();
    let resp = client
        .get(format!("{}/news", API_BASE))
        .bearer_auth(&token)
        .header("Accept", "application/json")
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        clear_token(&app, &state);
        return Err(AppError::Unauthenticated);
    }
    if !resp.status().is_success() {
        return Err(AppError::Unexpected(format!("HTTP {}", resp.status())));
    }
    Ok(resp.json().await?)
}

#[tauri::command]
fn is_logged_in(app: tauri::AppHandle, state: State<'_, AuthState>) -> bool {
    read_token(&app, &state).is_some()
}

#[derive(Serialize, Deserialize, Debug)]
struct SsoTicketResponse {
    url: String,
    #[serde(default)]
    expires_in: u32,
}

/// Demande au backend une URL signée à usage unique qui ouvrira une session
/// web Laravel pour l'utilisateur courant (sans qu'il ait à ressaisir ses
/// identifiants dans la WebView du jeu).
async fn request_sso_url(token: &str) -> AppResult<String> {
    let client = reqwest::Client::builder()
        .user_agent(format!("VelmoraDesktop/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(SSO_TIMEOUT_SECS))
        .build()
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let resp = client
        .post(format!("{}/sso/launcher-ticket", API_BASE))
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AppError::Unauthenticated);
    }
    if !resp.status().is_success() {
        return Err(AppError::Unexpected(format!(
            "SSO ticket HTTP {}",
            resp.status()
        )));
    }

    let parsed: SsoTicketResponse = resp.json().await?;
    Ok(parsed.url)
}

#[tauri::command]
async fn launch_game(app: tauri::AppHandle, state: State<'_, AuthState>) -> AppResult<()> {
    let token = read_token(&app, &state).ok_or(AppError::Unauthenticated)?;

    // Demande une URL signée au backend. En cas d'échec (réseau ou backend
    // sans endpoint SSO), on bascule sur l'URL de prod brute — l'utilisateur
    // verra le formulaire de login web.
    let target_url = match request_sso_url(&token).await {
        Ok(u) => u,
        Err(AppError::Unauthenticated) => {
            clear_token(&app, &state);
            return Err(AppError::Unauthenticated);
        }
        Err(e) => {
            eprintln!("SSO indisponible ({e}) — bascule sur URL de prod brute.");
            GAME_URL.to_string()
        }
    };

    let game = app
        .get_webview_window("game")
        .ok_or_else(|| AppError::Internal("fenêtre 'game' absente".into()))?;

    // Navigue la WebView vers l'URL signée (ou l'URL brute en fallback).
    let parsed = url::Url::parse(&target_url)
        .map_err(|e| AppError::Internal(format!("URL SSO invalide : {e}")))?;
    game.navigate(parsed)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    game.show().map_err(|e| AppError::Internal(e.to_string()))?;
    game.set_focus()
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if let Some(launcher) = app.get_webview_window("launcher") {
        let _ = launcher.hide();
    }
    Ok(())
}

#[tauri::command]
async fn show_launcher(app: tauri::AppHandle) -> AppResult<()> {
    if let Some(launcher) = app.get_webview_window("launcher") {
        launcher
            .show()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        launcher
            .set_focus()
            .map_err(|e| AppError::Internal(e.to_string()))?;
    }
    Ok(())
}

#[tauri::command]
fn game_url() -> String {
    GAME_URL.to_string()
}

#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------------- Auto-update ----------------

#[derive(Serialize, Clone)]
struct UpdateInfo {
    available: bool,
    version: Option<String>,
    notes: Option<String>,
}

#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> AppResult<UpdateInfo> {
    let updater = app
        .updater()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    match updater.check().await {
        Ok(Some(update)) => Ok(UpdateInfo {
            available: true,
            version: Some(update.version.clone()),
            notes: update.body.clone(),
        }),
        Ok(None) => Ok(UpdateInfo {
            available: false,
            version: None,
            notes: None,
        }),
        Err(e) => Err(AppError::Internal(e.to_string())),
    }
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> AppResult<()> {
    let updater = app
        .updater()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let update = updater
        .check()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::Internal("aucune mise à jour disponible".into()))?;

    let app_handle = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                let pct = total
                    .map(|t| (chunk as f64 / t as f64 * 100.0).round() as u32)
                    .unwrap_or(0);
                let _ = app_handle.emit("updater://progress", pct);
            },
            || {},
        )
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    app.restart();
}

// ---------------- Entry point ----------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AuthState::default())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("launcher") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            login,
            logout,
            me,
            dashboard,
            news,
            server_status,
            launcher_feed,
            get_setting,
            set_setting,
            is_first_run,
            prepare_first_run,
            recheck_manifest,
            get_autostart,
            set_autostart,
            is_logged_in,
            launch_game,
            show_launcher,
            game_url,
            app_version,
            check_for_updates,
            install_update,
        ])
        .setup(|app| {
            // Quand la fenêtre game se ferme, on ramène le launcher au premier plan.
            let app_handle = app.handle().clone();
            if let Some(game) = app.get_webview_window("game") {
                let handle = app_handle.clone();
                game.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { .. } = event {
                        if let Some(launcher) = handle.get_webview_window("launcher") {
                            let _ = launcher.show();
                            let _ = launcher.set_focus();
                        }
                    }
                });
            }

            // Polling background : toutes les 120 s, on appelle /api/mobile/dashboard
            // pour détecter une nouvelle missive et notifier nativement l'utilisateur
            // — même quand le launcher est minimisé. Le polling ne tourne que si un
            // token est présent ; il échoue silencieusement le reste du temps.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut last_missive_id: Option<i64> = None;
                let client = http_client();
                loop {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    let state: State<'_, AuthState> = handle.state();
                    let Some(token) = read_token(&handle, &state) else { continue };

                    let resp = match client
                        .get(format!("{}/dashboard", API_BASE))
                        .bearer_auth(&token)
                        .header("Accept", "application/json")
                        .send()
                        .await
                    {
                        Ok(r) if r.status().is_success() => r,
                        _ => continue,
                    };
                    let Ok(json): Result<serde_json::Value, _> = resp.json().await else { continue };
                    let Some(missives) = json.get("missives").and_then(|m| m.as_array()) else { continue };
                    let Some(first) = missives.first() else { continue };
                    let new_id = first.get("id").and_then(|v| v.as_i64());

                    if let (Some(prev), Some(new_id)) = (last_missive_id, new_id) {
                        if new_id > prev {
                            let sender = first
                                .get("sender")
                                .and_then(|s| s.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("Un seigneur");
                            let title = first
                                .get("subject")
                                .and_then(|t| t.as_str())
                                .unwrap_or("Nouvelle missive");
                            let _ = handle
                                .notification()
                                .builder()
                                .title(format!("Velmora — {}", title))
                                .body(format!("De : {}", sender))
                                .show();
                        }
                    }
                    if new_id.is_some() {
                        last_missive_id = new_id;
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("erreur au lancement de Velmora");
}

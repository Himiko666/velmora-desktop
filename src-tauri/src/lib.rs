use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tauri::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::WebviewBuilder,
    Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewUrl,
};
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

/// Cadence du pouls (poll /launcher-pulse) — 60 s : compromis réactivité / charge serveur.
const PULSE_INTERVAL_SECS: u64 = 60;

/// Hauteur (en points DP) de la title bar custom rendue par `src/game/index.html`.
/// Doit rester synchronisée avec la variable `--titlebar-h` du shell.
const GAME_TITLE_BAR_DP: f64 = 38.0;

/// Label du child webview qui rend velmora.cc à l'intérieur de la fenêtre `game`.
const GAME_CONTENT_LABEL: &str = "game-content";

/// Clés de persistance du curseur de pouls dans `velmora.json`.
const PULSE_CURSOR_KEY: &str = "pulse_cursor_at";
const PULSE_SEEN_CONSTRUCTIONS_KEY: &str = "pulse_seen_constructions";
const PULSE_LAST_QUESTS_DATE_KEY: &str = "pulse_last_quests_date";

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

/// Résumé partagé entre la boucle de pouls, le tray et le badge.
#[derive(Clone, Debug, Default)]
struct PulseSummary {
    missives_unread: u32,
    pending_constructions: u32,
    queued_construction_jobs: u32,
    incoming_battles: u32,
    quests_pending: u32,
    server_ok: bool,
}

impl PulseSummary {
    /// Compteur affiché en badge OS (missives + batailles subies non lues).
    fn badge_count(&self) -> u32 {
        self.missives_unread.saturating_add(self.incoming_battles)
    }
}

#[derive(Default)]
struct PulseState {
    summary: Mutex<PulseSummary>,
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

/// Calcule la taille en points logiques du child webview qui doit occuper la
/// zone sous la title bar custom. Retourne `None` si la fenêtre n'a pas de
/// taille mesurable (cas dégénéré, fenêtre minimisée juste avant le calcul).
fn content_logical_rect(game: &tauri::WebviewWindow) -> Option<(LogicalSize<f64>, LogicalPosition<f64>)> {
    let inner = game.inner_size().ok()?;
    let scale = game.scale_factor().unwrap_or(1.0).max(0.0001);
    let w = (inner.width as f64 / scale).max(1.0);
    let h = ((inner.height as f64 / scale) - GAME_TITLE_BAR_DP).max(1.0);
    Some((
        LogicalSize::new(w, h),
        LogicalPosition::new(0.0, GAME_TITLE_BAR_DP),
    ))
}

/// Repositionne le child webview `game-content` après un resize / maximize.
fn reposition_game_content(app: &tauri::AppHandle) {
    let Some(game) = app.get_webview_window("game") else { return };
    let Some(content) = app.get_webview(GAME_CONTENT_LABEL) else { return };
    let Some((size, pos)) = content_logical_rect(&game) else { return };
    let _ = content.set_position(pos);
    let _ = content.set_size(size);
}

/// Crée — ou réutilise — le child webview qui rend velmora.cc à l'intérieur
/// de la fenêtre `game`. Si le child existe déjà, on se contente d'y naviguer.
fn ensure_game_content(app: &tauri::AppHandle, url: url::Url) -> AppResult<()> {
    let game = app
        .get_webview_window("game")
        .ok_or_else(|| AppError::Internal("fenêtre 'game' absente".into()))?;

    if let Some(content) = app.get_webview(GAME_CONTENT_LABEL) {
        content
            .navigate(url)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        return Ok(());
    }

    let (size, pos) = content_logical_rect(&game)
        .ok_or_else(|| AppError::Internal("taille fenêtre game indisponible".into()))?;

    let app_for_event = app.clone();
    let builder = WebviewBuilder::new(GAME_CONTENT_LABEL, WebviewUrl::External(url))
        .on_page_load(move |_webview, _payload| {
            let _ = app_for_event.emit("game-content-loaded", ());
        });

    game.add_child(builder, pos, size)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(())
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

    let parsed = url::Url::parse(&target_url)
        .map_err(|e| AppError::Internal(format!("URL SSO invalide : {e}")))?;

    let game = app
        .get_webview_window("game")
        .ok_or_else(|| AppError::Internal("fenêtre 'game' absente".into()))?;

    // On affiche d'abord la fenêtre (avec son shell + splash) pour que
    // l'utilisateur ait un retour visuel pendant que le child webview se monte.
    game.show().map_err(|e| AppError::Internal(e.to_string()))?;
    game.set_focus()
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Monte ou ré-utilise le child webview qui héberge velmora.cc.
    ensure_game_content(&app, parsed)?;

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

// ---------------- Pouls du launcher ----------------

/// Lit les ids de chantiers déjà connus au tick précédent (persistés en JSON).
fn read_seen_construction_ids(app: &tauri::AppHandle) -> std::collections::HashSet<i64> {
    let mut set = std::collections::HashSet::new();
    let Ok(store) = app.store(STORE_FILE) else { return set };
    let Some(value) = store.get(PULSE_SEEN_CONSTRUCTIONS_KEY) else { return set };
    if let Some(arr) = value.as_array() {
        for v in arr {
            if let Some(id) = v.as_i64() {
                set.insert(id);
            }
        }
    }
    set
}

fn write_seen_construction_ids(app: &tauri::AppHandle, ids: &std::collections::HashSet<i64>) {
    let Ok(store) = app.store(STORE_FILE) else { return };
    let arr: Vec<serde_json::Value> = ids
        .iter()
        .map(|i| serde_json::Value::from(*i))
        .collect();
    store.set(PULSE_SEEN_CONSTRUCTIONS_KEY, serde_json::Value::Array(arr));
    let _ = store.save();
}

fn read_string_setting(app: &tauri::AppHandle, key: &str) -> Option<String> {
    let store = app.store(STORE_FILE).ok()?;
    store.get(key).and_then(|v| v.as_str().map(|s| s.to_string()))
}

fn write_string_setting(app: &tauri::AppHandle, key: &str, value: &str) {
    let Ok(store) = app.store(STORE_FILE) else { return };
    store.set(key, serde_json::Value::String(value.to_string()));
    let _ = store.save();
}

/// Un tick de polling : récupère le pouls, détecte les transitions,
/// notifie, met à jour le résumé partagé (tray + badge).
async fn pulse_tick(
    app: &tauri::AppHandle,
    client: &reqwest::Client,
) -> Result<(), AppError> {
    let auth: State<'_, AuthState> = app.state();
    let Some(token) = read_token(app, &auth) else {
        return Ok(());
    };

    let since = read_string_setting(app, PULSE_CURSOR_KEY);
    let mut req = client
        .get(format!("{}/launcher-pulse", API_BASE))
        .bearer_auth(&token)
        .header("Accept", "application/json");
    if let Some(s) = since.as_ref() {
        req = req.query(&[("since", s.as_str())]);
    }

    let resp = req
        .send()
        .await
        .map_err(AppError::from)?;
    if !resp.status().is_success() {
        return Err(AppError::Unexpected(format!("pulse HTTP {}", resp.status())));
    }
    let json: serde_json::Value = resp.json().await.map_err(AppError::from)?;

    // Notifs (uniquement si on avait un curseur précédent — cold start silencieux).
    let cold_start = since.is_none();

    // 1. Missives reçues depuis la dernière vue.
    if !cold_start {
        if let Some(arr) = json.get("missives_recent").and_then(|v| v.as_array()) {
            for m in arr {
                let sender = m.get("sender").and_then(|s| s.as_str()).unwrap_or("Un seigneur");
                let subject = m.get("subject").and_then(|s| s.as_str()).unwrap_or("Nouvelle missive");
                let _ = app.notification().builder()
                    .title(format!("Velmora — {}", subject))
                    .body(format!("De : {}", sender))
                    .show();
            }
        }

        // 2. Batailles subies depuis la dernière vue.
        if let Some(arr) = json.get("battles_received").and_then(|v| v.as_array()) {
            for b in arr {
                let attacker = b.get("attacker").and_then(|s| s.as_str()).unwrap_or("Un assaillant");
                let result = b.get("result").and_then(|s| s.as_str()).unwrap_or("");
                let _ = app.notification().builder()
                    .title("Velmora — Forteresse attaquée")
                    .body(format!("{} vous a assailli ({})", attacker, result))
                    .show();
            }
        }
    }

    // 3. Chantiers terminés : id présent au tick précédent, absent maintenant.
    let prev_ids = read_seen_construction_ids(app);
    let mut current_ids = std::collections::HashSet::new();
    let mut current_labels: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    if let Some(arr) = json.get("pending_constructions").and_then(|v| v.as_array()) {
        for c in arr {
            if let Some(id) = c.get("id").and_then(|v| v.as_i64()) {
                current_ids.insert(id);
                if let Some(label) = c.get("label").and_then(|v| v.as_str()) {
                    current_labels.insert(id, label.to_string());
                }
            }
        }
    }
    if !cold_start {
        for finished_id in prev_ids.difference(&current_ids) {
            let label = current_labels
                .get(finished_id)
                .cloned()
                .unwrap_or_else(|| "Chantier".to_string());
            let _ = app.notification().builder()
                .title("Velmora — Chantier terminé")
                .body(label)
                .show();
        }
    }
    write_seen_construction_ids(app, &current_ids);

    // 4. Quêtes du jour fraîchement disponibles (rollover de date).
    let quests_today_pending = json
        .get("quests_today_pending")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let quests_today_date = json
        .get("quests_today_date")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let last_quests_date = read_string_setting(app, PULSE_LAST_QUESTS_DATE_KEY);
    let date_rolled = last_quests_date
        .as_ref()
        .map(|d| d != &quests_today_date)
        .unwrap_or(true);
    if !cold_start && date_rolled && quests_today_pending > 0 {
        let _ = app.notification().builder()
            .title("Velmora — Quêtes du jour")
            .body(format!("{} quête(s) disponible(s) au royaume", quests_today_pending))
            .show();
    }
    if !quests_today_date.is_empty() {
        write_string_setting(app, PULSE_LAST_QUESTS_DATE_KEY, &quests_today_date);
    }

    // Persiste le curseur (snapshot_at fourni par le serveur — source de vérité).
    if let Some(snap) = json.get("snapshot_at").and_then(|v| v.as_str()) {
        write_string_setting(app, PULSE_CURSOR_KEY, snap);
    }

    // Met à jour le résumé partagé (utilisé par le tray + badge).
    let summary = PulseSummary {
        missives_unread: json.get("missives_unread").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        pending_constructions: current_ids.len() as u32,
        queued_construction_jobs: json.get("queued_construction_jobs").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        incoming_battles: json.get("battles_received").and_then(|v| v.as_array()).map(|a| a.len() as u32).unwrap_or(0),
        quests_pending: quests_today_pending as u32,
        server_ok: json.get("server_ok").and_then(|v| v.as_bool()).unwrap_or(true),
    };
    let pulse_state: State<'_, PulseState> = app.state();
    if let Ok(mut g) = pulse_state.summary.lock() {
        *g = summary.clone();
    }
    refresh_tray_and_badge(app, &summary);

    // Émet un évènement pour rafraîchir le UI de la fenêtre launcher.
    let _ = app.emit("launcher://pulse", json);

    Ok(())
}

/// Construit le menu contextuel du tray à partir du résumé courant.
fn build_tray_menu(app: &tauri::AppHandle, summary: &PulseSummary) -> Menu<tauri::Wry> {
    let status_label = if summary.server_ok {
        "Statut serveur · en ligne"
    } else {
        "Statut serveur · indisponible"
    };
    let menu = Menu::new(app).expect("create tray menu");
    let _ = menu.append(&MenuItem::with_id(app, "tray-open", "Ouvrir Velmora", true, None::<&str>).expect("menu open"));
    let _ = menu.append(&MenuItem::with_id(app, "tray-status", status_label, false, None::<&str>).expect("menu status"));
    let _ = menu.append(&MenuItem::with_id(
        app,
        "tray-missives",
        format!("Missives non lues · {}", summary.missives_unread),
        false,
        None::<&str>,
    ).expect("menu missives"));
    let _ = menu.append(&MenuItem::with_id(
        app,
        "tray-constructions",
        format!("Chantiers en cours · {} (file +{})", summary.pending_constructions, summary.queued_construction_jobs),
        false,
        None::<&str>,
    ).expect("menu constructions"));
    if summary.quests_pending > 0 {
        let _ = menu.append(&MenuItem::with_id(
            app,
            "tray-quests",
            format!("Quêtes du jour · {}", summary.quests_pending),
            false,
            None::<&str>,
        ).expect("menu quests"));
    }
    let _ = menu.append(&PredefinedMenuItem::separator(app).expect("menu sep"));
    let _ = menu.append(&MenuItem::with_id(app, "tray-quit", "Quitter Velmora", true, None::<&str>).expect("menu quit"));
    menu
}

/// Réagit aux clics dans le menu tray.
fn handle_tray_menu_event(app: &tauri::AppHandle, event: MenuEvent) {
    match event.id.as_ref() {
        "tray-open" => focus_best_window(app),
        "tray-quit" => app.exit(0),
        _ => {}
    }
}

/// Met le focus sur la fenêtre la plus pertinente (game si visible, sinon launcher).
fn focus_best_window(app: &tauri::AppHandle) {
    if let Some(game) = app.get_webview_window("game") {
        if game.is_visible().unwrap_or(false) {
            let _ = game.unminimize();
            let _ = game.show();
            let _ = game.set_focus();
            return;
        }
    }
    if let Some(launcher) = app.get_webview_window("launcher") {
        let _ = launcher.unminimize();
        let _ = launcher.show();
        let _ = launcher.set_focus();
    }
}

/// Met à jour le menu tray et le badge OS à partir d'un nouveau résumé.
fn refresh_tray_and_badge(app: &tauri::AppHandle, summary: &PulseSummary) {
    // Badge : missives non lues + batailles subies récentes.
    let count = summary.badge_count();
    if let Some(win) = app.get_webview_window("launcher") {
        let value = if count == 0 { None } else { Some(count as i64) };
        let _ = win.set_badge_count(value);
    }

    // Mise à jour des libellés du menu tray.
    if let Some(tray) = app.tray_by_id("velmora-tray") {
        let _ = tray.set_menu(Some(build_tray_menu(app, summary)));
    }
}

// ---------------- Entry point ----------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AuthState::default())
        .manage(PulseState::default())
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
            // Quand elle est redimensionnée, on repositionne le child webview qui
            // héberge velmora.cc sous la title bar custom.
            let app_handle = app.handle().clone();
            if let Some(game) = app.get_webview_window("game") {
                let handle = app_handle.clone();
                game.on_window_event(move |event| match event {
                    tauri::WindowEvent::CloseRequested { .. } => {
                        if let Some(launcher) = handle.get_webview_window("launcher") {
                            let _ = launcher.show();
                            let _ = launcher.set_focus();
                        }
                    }
                    tauri::WindowEvent::Resized(_)
                    | tauri::WindowEvent::ScaleFactorChanged { .. } => {
                        reposition_game_content(&handle);
                    }
                    _ => {}
                });
            }

            // Tray icon natif (Tauri 2) — toujours présent, permet de revenir
            // au launcher / game même après fermeture des fenêtres.
            let tray_app = app.handle().clone();
            let initial_menu = build_tray_menu(&tray_app, &PulseSummary { server_ok: true, ..PulseSummary::default() });
            let _ = TrayIconBuilder::with_id("velmora-tray")
                .icon(app.default_window_icon().cloned().expect("default icon"))
                .tooltip("Velmora")
                .menu(&initial_menu)
                .on_menu_event(|app, event| handle_tray_menu_event(app, event))
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        focus_best_window(tray.app_handle());
                    }
                })
                .build(app);

            // Polling background : toutes les PULSE_INTERVAL_SECS, on appelle
            // /api/mobile/launcher-pulse (endpoint compact dédié) pour détecter
            // les 4 types d'évènements à notifier nativement :
            //   - nouvelle missive
            //   - chantier terminé (id présent au tick N, absent au tick N+1)
            //   - bataille subie
            //   - quêtes du jour fraîchement disponibles
            // L'état (curseur ISO, ids déjà vus, dernière date de quêtes) est
            // persisté dans le store pour survivre aux redémarrages — anti
            // re-notif après reboot.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let client = http_client();
                loop {
                    tokio::time::sleep(Duration::from_secs(PULSE_INTERVAL_SECS)).await;
                    let _ = pulse_tick(&handle, &client).await;
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("erreur au lancement de Velmora");
}

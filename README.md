# Velmora Desktop — Launcher

Launcher natif pour [Velmora](https://velmora.cc) : interface dédiée pour se connecter, consulter ses ressources et les actualités du royaume, puis lancer le jeu dans une fenêtre séparée. Conçu avec **Tauri 2** (Rust + WebView système), distribué en `.exe`, `.dmg`, `.AppImage` et `.deb`.

> Le launcher consomme directement l'API mobile existante (`/api/mobile/*`) du backend Laravel — aucun service supplémentaire à déployer côté serveur (sauf l'endpoint d'auto-update, voir plus bas).

## Aperçu

L'application embarque **deux fenêtres natives distinctes** :

| Fenêtre | Contenu | Affichée |
| --- | --- | --- |
| `launcher` | UI native (login, sidebar profil + onglets News/Patch/Évènements, quick links Forum/Discord/Codex/Soutien, statut serveur live, paramètres, bouton JOUER, bandeau MAJ) | au démarrage |
| `game` | WebView du jeu (`https://velmora.cc`), session web ouverte automatiquement via SSO | au clic JOUER |

Au clic JOUER, le launcher demande au backend une **URL signée à usage unique** (TTL 60 s), navigue la fenêtre `game` dessus, ce qui ouvre la session web Laravel — sans que l'utilisateur ait à ressaisir ses identifiants. Le launcher se masque ; quand la fenêtre de jeu est fermée, le launcher revient.

> **Périmètre volontairement restreint** : le launcher n'affiche **pas** les ressources, chantiers ou missives. Ces écrans vivent dans le jeu. Le launcher sert à se connecter, lire les news, mettre à jour le binaire et entrer en jeu.

## Première installation — flux complet (2 phases)

Distribution « launcher MMO » : l'utilisateur télécharge un mini-installeur, qui télécharge le vrai launcher, qui prépare ensuite l'environnement de jeu.

```
┌──────────────────────────┐     ┌──────────────────────────┐     ┌──────────────────────────┐
│  1. STUB INSTALLER       │     │  2. LAUNCHER (1er run)   │     │  3. LAUNCHER (n+1 run)   │
│  velmora-desktop-stub/   │     │  setup screen            │     │  écran login direct      │
│                          │     │                          │     │                          │
│  - 480×360, ~5-8 Mo      │     │  - Vérif royaume         │     │  - Pastille statut       │
│  - GET latest.json       │     │  - GET asset-manifest    │     │  - News / Patch / Events │
│  - Télécharge .msi/.dmg/ │ ──▶ │  - Pré-cache assets      │ ──▶ │  - Quick links           │
│    .AppImage selon OS    │     │  - Marque setup_done     │     │  - JOUER (SSO)           │
│  - Lance l'installeur    │     │  - Bascule sur login     │     │  - Updater auto          │
│  - Se ferme              │     │                          │     │                          │
└──────────────────────────┘     └──────────────────────────┘     └──────────────────────────┘
```

**Phase 1 — Stub installer (`velmora-desktop-stub/`)**
- Téléchargé depuis `velmora.cc/desktop` par l'utilisateur.
- Lit `https://velmora.cc/desktop/latest.json`, détecte la plateforme (`windows-x86_64`, `darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`).
- Télécharge en streaming le `.msi` / `.dmg` / `.AppImage` / `.deb` adéquat avec barre de progression.
- Lance l'installeur natif puis se ferme. C'est ça la différence : pas besoin de re-télécharger un installeur entier à chaque update, le stub fait toujours le bon choix de version.

**Phase 2 — Setup screen au premier lancement du launcher**
- Détecté côté Rust via `is_first_run()` (clé `setup_completed` absente du store).
- Affiche la vue `view-setup` : 4 étapes coches → vérif serveur, manifeste, pré-cache assets, configuration.
- Backend `GET /api/mobile/asset-manifest` liste les assets statiques à pré-cacher (logos, forteresses, blasons…).
- Le launcher télécharge chaque asset pour chauffer le cache WebView → premier JOUER instantané.
- Marque `setup_completed = true` et bascule sur l'écran login.

**Lancements suivants** : `is_first_run() == false`, le launcher saute le setup et va direct sur le login (ou home si token persistant valide).

## Stack

| Brique | Choix |
| --- | --- |
| Wrapper natif | [Tauri 2.x](https://v2.tauri.app/) |
| Backend natif | Rust stable (1.77+) — `reqwest` pour l'API, `tauri-plugin-store` pour le token, `tauri-plugin-updater` pour les MAJ |
| Frontend launcher | Vanilla HTML/CSS/JS (zéro framework) |
| WebView | WebKitGTK (Linux), WKWebView (macOS), WebView2 (Windows) |
| Distribution | GitHub Actions multi-OS, draft release sur tag |
| Taille `.deb` | ~3 Mo |
| Taille `.AppImage` | ~100 Mo (WebKit embarqué) |

## Architecture

```
velmora-desktop/
├── package.json              # CLI Tauri (devDep uniquement)
├── src/
│   ├── launcher/
│   │   ├── index.html        # UI native du launcher
│   │   ├── styles.css        # Palette médiéval-fantastique (or/charbon/parchemin)
│   │   └── launcher.js       # Appels à invoke() → backend Rust
│   └── game/
│       └── index.html        # Splash + redirect vers velmora.cc
├── src-tauri/
│   ├── Cargo.toml            # Plugins : updater, store, opener, single-instance, notification, process
│   ├── tauri.conf.json       # 2 fenêtres (launcher + game) + bundle config
│   ├── capabilities/
│   │   └── default.json      # Permissions accordées aux fenêtres
│   ├── icons/                # Générées depuis pwa-512.png
│   └── src/
│       ├── main.rs
│       └── lib.rs            # Commands : login, dashboard, news, launch_game, check_for_updates…
└── .github/workflows/release.yml
```

### Commandes Rust exposées au frontend

| Commande | Description |
| --- | --- |
| `login(email, password)` | Appelle `POST /api/mobile/login`, stocke le token via `tauri-plugin-store` |
| `logout()` | Appelle `POST /api/mobile/logout` puis efface le token local |
| `me()` | `GET /api/mobile/me` — identité du seigneur affichée dans le launcher |
| `server_status()` | `GET /api/mobile/status` (public) — pastille statut + compteur joueurs en ligne + prochaine maintenance |
| `launcher_feed()` | `GET /api/mobile/launcher-feed` — agrège news + patch notes + évènements + quick links en un appel |
| `news()` | `GET /api/mobile/news` (legacy, conservé) |
| `get_setting(key)` / `set_setting(key, value)` | Préférences locales persistées dans `settings.json` via `tauri-plugin-store` |
| `get_autostart()` / `set_autostart(enabled)` | Active/désactive le démarrage automatique au boot OS (`tauri-plugin-autostart`) |
| `is_logged_in()` | Vrai si un token est présent localement |
| `launch_game()` | Demande une URL SSO signée à `POST /api/mobile/sso/launcher-ticket`, navigue la fenêtre `game` dessus, la révèle, masque le launcher. En cas d'échec SSO, bascule sur `https://velmora.cc` brut (formulaire login web) |
| `show_launcher()` | Ramène le launcher au premier plan |

### Background : notifications natives

Une tâche tokio démarre au boot du launcher et appelle `GET /api/mobile/dashboard` toutes les **120 s** (uniquement si un token est présent). Elle compare l'ID de la missive la plus récente avec celle gardée en mémoire — si une nouvelle missive est arrivée, elle envoie une notification système (`tauri-plugin-notification`), même quand le launcher est minimisé. Échec silencieux le reste du temps.
| `app_version()` | Version du launcher (depuis `Cargo.toml`) |
| `check_for_updates()` | Vérifie via `tauri-plugin-updater` si une MAJ est dispo |
| `install_update()` | Télécharge + installe la MAJ, puis redémarre l'app |

### Authentification — flux complet

1. **Login native** : le launcher appelle `POST /api/mobile/login` et reçoit un **token mobile** (80 chars, valable 365 jours, max 5 tokens par compte). Stocké chiffré dans `tauri-plugin-store`.
2. **Requêtes mobiles** : toutes les requêtes ultérieures envoient `Authorization: Bearer <token>` (identité, news, ticket SSO).
3. **SSO → webview** : au clic JOUER, le launcher appelle `POST /api/mobile/sso/launcher-ticket` (auth Bearer). Le backend :
   - Génère un ticket aléatoire 64 chars
   - Stocke `hash(ticket)` → `user_id` en cache Laravel, TTL 60 s
   - Retourne une URL signée (`URL::temporarySignedRoute`) du type `https://velmora.cc/launcher-sso/<ticket>?signature=…&expires=…`
4. **Consommation** : la fenêtre `game` navigue vers cette URL. La route web `launcher.sso.consume` (middleware `signed`) :
   - Récupère le ticket via `Cache::pull` (atomique, usage unique)
   - Charge l'utilisateur, vérifie qu'il n'est pas banni et que l'e-mail est vérifié
   - Régénère la session et appelle `Auth::login($user, remember: true)`
   - Redirige vers `/dashboard`
5. **Fenêtre de jeu** : l'utilisateur arrive dans le jeu avec une session web active, sans avoir ressaisi son mot de passe.

Sécurité : URL signée + TTL 60 s côté cache + consommation atomique + `throttle:20,1` côté consumer.

Côté serveur, le code SSO + endpoints launcher vivent dans :
- `app/Http/Controllers/Mobile/LauncherSsoController.php` — émet le ticket signé
- `app/Http/Controllers/Auth/LauncherSsoConsumeController.php` — consomme et ouvre la session
- `app/Http/Controllers/Mobile/LauncherStatusController.php` — statut serveur public
- `app/Http/Controllers/Mobile/LauncherFeedController.php` — news + patch + events + quick links
- `config/velmora.php` — config `launcher.quick_links`, `next_maintenance_at`, `online_window_minutes`, chemins JSON
- `storage/app/launcher/patch-notes.json` + `events.json` — alimentés à la main ou par un cron
- routes : `routes/api.php` (`mobile.status`, `mobile.launcher_feed`, `mobile.sso.launcher_ticket`) + `routes/web.php` (`launcher.sso.consume`)

## Développement local

### Prérequis

- **Rust** stable (1.77+) : `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node 20+**
- **Linux** :
  ```bash
  sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
    librsvg2-dev libssl-dev patchelf libsoup-3.0-dev pkg-config build-essential
  ```
- **macOS** : `xcode-select --install`
- **Windows** : [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) + Visual Studio Build Tools

### Setup

```bash
npm install
npm run dev      # lance le launcher en mode dev (hot-reload Rust)
npm run build    # build de release pour la plateforme courante
```

Les binaires apparaissent dans `src-tauri/target/release/bundle/` :

| OS | Sorties |
| --- | --- |
| Linux | `*.AppImage`, `*.deb`, `*.rpm` |
| macOS | `*.dmg`, `*.app` |
| Windows | `*.msi`, `*-setup.exe` (NSIS) |

## Release multi-OS (GitHub Actions)

Pousser un tag déclenche le build sur Ubuntu / macOS (Intel + Silicon) / Windows et crée un **draft release** GitHub avec tous les installeurs attachés :

```bash
git tag v0.1.0
git push origin v0.1.0
```

Workflow : [`.github/workflows/release.yml`](.github/workflows/release.yml)

Tu peux aussi déclencher le workflow à la main depuis l'onglet **Actions** (`workflow_dispatch`) pour récupérer les binaires en tant qu'artefacts, sans créer de release.

## Auto-update (à finaliser)

L'app inclut déjà `tauri-plugin-updater`. Pour activer :

### 1. Générer la paire de clés (à faire une seule fois, garder la clé privée hors repo)

```bash
npm run tauri signer generate -- -w ~/.tauri/velmora-updater.key
```

Récupère la **clé publique** affichée à la fin.

### 2. Mettre à jour `src-tauri/tauri.conf.json`

Remplace `REMPLACE_PAR_CLE_PUBLIQUE_GENEREE_AVEC_tauri_signer_generate` par la clé publique.

### 3. Côté serveur Laravel — exposer un endpoint `latest.json`

Créer une route publique `/desktop/latest.json` qui retourne :

```json
{
  "version": "0.2.0",
  "notes": "Notes de version",
  "pub_date": "2026-06-01T12:00:00Z",
  "platforms": {
    "linux-x86_64":   { "signature": "…", "url": "https://velmora.cc/desktop/v0.2.0/Velmora_0.2.0_amd64.AppImage" },
    "darwin-aarch64": { "signature": "…", "url": "https://velmora.cc/desktop/v0.2.0/Velmora_0.2.0_aarch64.app.tar.gz" },
    "darwin-x86_64":  { "signature": "…", "url": "https://velmora.cc/desktop/v0.2.0/Velmora_0.2.0_x64.app.tar.gz" },
    "windows-x86_64": { "signature": "…", "url": "https://velmora.cc/desktop/v0.2.0/Velmora_0.2.0_x64-setup.nsis.zip" }
  }
}
```

Le launcher consultera ce fichier à chaque démarrage, vérifiera la signature Ed25519, et proposera l'installation via un bandeau en haut de l'UI.

### 4. Workflow CI — secrets à ajouter

Décommenter dans `.github/workflows/release.yml` :

```yaml
TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
```

Et ajouter les secrets correspondants dans GitHub.

## Signature de code (optionnel mais recommandé)

Sans certificat :
- **Windows** → SmartScreen affiche "éditeur inconnu" (installable mais effrayant)
- **macOS** → Gatekeeper bloque, l'utilisateur doit faire clic-droit > Ouvrir
- **Linux** → aucun warning, RAS

Pour signer :
- **macOS** : Apple Developer Program (~99 $/an) — Developer ID Application certificate
- **Windows** : EV Code Signing Certificate (~200 €/an) — Sectigo, DigiCert, etc.

Renseigner ensuite les secrets `APPLE_*` dans GitHub et décommenter les `env:` correspondants dans le workflow.

## Licence

À définir (le launcher ne contient pas de code propriétaire du jeu).

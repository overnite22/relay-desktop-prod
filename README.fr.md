[English](README.md) · **Français**

# Relay Desktop (Windows)

Client Windows natif pour Relay, écrit en Rust avec une interface [Slint](https://slint.dev)
(sans vue web). Distribué sous forme de MSI.

## Sources de données

Comme les sources de données de RDM, plusieurs peuvent être déclarées et sélectionnées depuis le ruban :

- **Serveur Relay** : l'inventaire, le coffre, l'héritage des identifiants par dossier et les notes
  proviennent d'un serveur Relay, authentifié par un **jeton d'appareil** créé dans
  Relay web → Administration → Appareils. Le jeton est stocké dans le Gestionnaire
  d'informations d'identification de Windows, jamais dans le fichier de paramètres. HTTPS est
  obligatoire (sauf boucle locale). Si un proxy SSO protège Relay, il doit laisser passer les requêtes `/api/`
  portant un jeton d'appareil.
- **Base locale** : un simple fichier SQLite, aucun serveur nécessaire. Les secrets des
  identifiants sont chiffrés en AES-256-GCM avec une clé dérivée du mot de passe maître
  (Argon2id, 64 Mio, 3 passes). L'inventaire (noms, hôtes, notes) est
  stocké en clair, comme une source de données locale RDM.

Paramètres : `%APPDATA%\Relay\settings.json` (liste des sources uniquement).

## Sessions

Version actuelle : RDP via `mstsc` (l'identifiant est transmis sous forme d'entrée
`TERMSRV/<host>` valable pour la session seulement, supprimée après 30 s, sans jamais écraser une entrée existante),
SSH via le client OpenSSH de Windows, consoles web dans le navigateur par défaut.
Les onglets RDP/SSH intégrés sont les prochaines étapes (voir `PLAN.md`).

Clavier : ↑/↓ déplacent, →/← déplient/replient, Entrée ouvre, F2 modifie, Suppr supprime.

## Compilation

```sh
cargo test -p relay-desktop-core                       # any platform
cargo build --release -p relay-desktop                 # Windows (MSVC)
wix build app/wix/Relay.wxs -arch x64 -d Version=… -d ExePath=…   # MSI, WiX 5
```

La CI (`.github/workflows/desktop.yml`) compile le MSI sur `windows-latest`.

## Mises à jour

**Aide → Rechercher des mises à jour** (ainsi qu'une vérification silencieuse au démarrage, au plus
une fois par jour, désactivable dans la même boîte de dialogue) consulte les versions publiées du dépôt public
`relay-desktop-prod`. Lorsqu'une version plus récente existe, Relay
télécharge `Relay-X.Y.Z-x64.msi`, le vérifie avec le fichier `SHA256SUMS` publié
(et, une fois Relay signé, exige le même éditeur Authenticode),
lance `msiexec /passive` et redémarre après l'installation. Les paramètres de proxy
proviennent de la variable d'environnement `HTTPS_PROXY`.

## Publier une version (mainteneur)

1. Définissez la version dans `Cargo.toml` (`[workspace.package] version`), validez et
   poussez sur `main` du dépôt de développement ; attendez une CI au vert.
2. Facultatif : posez le tag `desktop-vX.Y.Z` dans le dépôt de développement pour obtenir le MSI
   comme artefact de CI (signé via SignPath lorsque configuré).
3. `scripts/publish-prod.sh X.Y.Z [notes.md]` prépare un instantané propre (sans
   historique, sources de compilation uniquement), y recherche des données privées et lance gitleaks.
   Vérifiez l'aperçu, puis relancez-le avec `--push`.
4. Le workflow `Release` de `relay-desktop-prod` compile le MSI à partir des
   sources publiques et publie la version ; les clients installés la voient alors.

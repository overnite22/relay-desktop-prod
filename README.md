**English** · [Français](README.fr.md)

# Relay Desktop (Windows)

Native Windows client for Relay, written in Rust with a [Slint](https://slint.dev)
interface (no web view). Shipped as an MSI.

## Data sources

Like RDM data sources, several can be declared and switched from the ribbon:

- **Relay server**: inventory, vault, folder credential inheritance and notes
  come from a Relay server, authenticated with a **device token** created in
  Relay web → Administration → Appareils. The token is stored in the Windows
  Credential Manager, never in the settings file. HTTPS is required (except
  loopback). If an SSO proxy protects Relay, it must let `/api/` requests with
  a device token through.
- **Local database**: a single SQLite file, no server needed. Credential
  secrets are encrypted with AES-256-GCM using a key derived from the master
  password (Argon2id, 64 MiB, 3 passes). Inventory (names, hosts, notes) is
  stored in clear, like an RDM local data source.

Settings: `%APPDATA%\Relay\settings.json` (source list only).

## Sessions

Current version: RDP through `mstsc` (credential passed as a session-only
`TERMSRV/<host>` entry removed after 30 s, never overwriting an existing one),
SSH through the Windows OpenSSH client, web consoles in the default browser.
Embedded RDP/SSH tabs are the next steps (see `PLAN.md`).

Keyboard: ↑/↓ move, →/← expand/collapse, Enter opens, F2 edits, Del deletes.

## Build

```sh
cargo test -p relay-desktop-core                       # any platform
cargo build --release -p relay-desktop                 # Windows (MSVC)
wix build app/wix/Relay.wxs -arch x64 -d Version=… -d ExePath=…   # MSI, WiX 5
```

CI (`.github/workflows/desktop.yml`) builds the MSI on `windows-latest`.

## Updates

**Aide → Rechercher des mises à jour** (and a silent check at startup, at most
once a day, which can be turned off in the same dialog) reads the releases of
the public `relay-desktop-prod` repository. When a newer version exists, Relay
downloads `Relay-X.Y.Z-x64.msi`, checks it against the published `SHA256SUMS`
(and, once Relay is code-signed, requires the same Authenticode publisher),
starts `msiexec /passive` and restarts after installation. Proxy settings
come from the `HTTPS_PROXY` environment variable.

## Publishing a version (maintainer)

1. Set the version in `Cargo.toml` (`[workspace.package] version`), commit and
   push to `main` of the development repository; wait for a green CI.
2. Optional: tag `desktop-vX.Y.Z` in the development repository to get the MSI
   as a CI artifact (signed through SignPath when configured).
3. `scripts/publish-prod.sh X.Y.Z [notes.md]` prepares a clean snapshot (no
   history, build sources only), scans it for private data and runs gitleaks.
   Check the preview, then run it again with `--push`.
4. The `Release` workflow of `relay-desktop-prod` builds the MSI from the
   public source and publishes the release; installed clients then see it.

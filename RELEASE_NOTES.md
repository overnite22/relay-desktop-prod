Relay Desktop 0.1.2: per-entry connection settings, an Options window and many fixes from testing on Windows Server 2025.

- Per-entry connection settings: RDP resolution (fit to tab, fixed size, full screen), colors, multi-monitor, clipboard, drives, printers, sound, RD Gateway, console session, security; SSH font, size, color scheme, scrollback, keepalive, SSH agent and key file
- Options window (Help → Options): theme, startup update check, default terminal font and size, default RDP resolution
- NetBox / Proxmox sync sources: create, edit and delete natively
- Local SSH session recording (asciicast v2) and built-in player
- RDP: a refused password shows the disconnect reason with "Reconnect" instead of a Windows credential prompt; no more stale white band after a session; menus and dialogs stay visible over a session; keyboard focus returns to the session after Alt+Tab
- Session tabs scroll when they overflow, with arrows and a list of all open tabs
- Notes: bare http(s) URLs are clickable, task check boxes are visible, the whole entry sheet scrolls so notes and attachments get the full height
- Terminal: row backgrounds no longer cover text with fonts such as Lucida Console
- Administration lists: actions that need a selected row stay disabled until one is chosen
- The ribbon scrolls with the mouse wheel on small screens; dates shown in local time

The MSI is not code-signed yet: Windows SmartScreen may show a warning.

---

Relay Desktop 0.1.2 : réglages de connexion par entrée (RDP et SSH), fenêtre Options, sources de synchro NetBox / Proxmox natives, enregistrement et relecture des sessions SSH locales, et de nombreuses corrections issues des tests sur Windows Server 2025 (mot de passe RDP refusé sans invite Windows, onglets qui défilent, URL cliquables dans les notes, rendu du terminal, boutons inactifs sans sélection). Le MSI n'est pas encore signé : SmartScreen peut afficher un avertissement.

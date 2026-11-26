# MineHost 1.0.0

> **Language / Язык:** [Русская версия](README.md) · **English**

A manager for local Minecraft Java servers on Windows 11 x64. Servers, worlds, and settings stay on your computer. MineHost does not upload server files to GitHub Actions.

## Installation

Download `MineHost_1.0.0_x64-setup.exe` from [GitHub Releases](https://github.com/Frish07/MineHost/releases/tag/v1.0.0) and compare its SHA-256 checksum with `SHA256SUMS.txt` from the same release. If Windows blocks the download, check that the installer came from the official release and verify its checksum. Do not bypass warnings for files from untrusted sources. The installer does not require administrator privileges for a per-user installation. If Windows asks for a password, verify the installer path and publisher before continuing.

**You do not need to install Rust, Cargo, Node.js, npm, the Tauri CLI, Visual Studio, or a JDK to use MineHost. MineHost does not install these tools.** A Java runtime is required to run Minecraft servers. MineHost can download a compatible Temurin JRE and verify it using the published SHA-256 checksum. You may also set `PATH` or `JAVA_HOME` to use an existing Java installation.

`MineHost-windows-x64.exe` is also available for running the app without an installer. WebView2 is required; it is usually already available on Windows 11. If it is missing, install the Microsoft WebView2 Runtime. The portable EXE does not install the runtime for you.

The owner has postponed app signing indefinitely. SmartScreen may show an “Unknown publisher” warning. The signing policy is documented in [CODE_SIGNING_POLICY.md](CODE_SIGNING_POLICY.md).

## Server management

Select a server card to open its overview and sidebar. Returning to “My Servers” closes the workspace and stops displaying the selected server’s management view.

- Console, log search and filters, command history, graceful stop/restart, and optional, limited recovery after an unexpected shutdown.
- Monitoring of Java’s actual CPU and RAM usage, uptime, and manually queried TPS/MSPT through compatible spark. [Details](docs/MONITORING.md).
- A form for common `server.properties` settings and an advanced editor with Java escapes, comments, unknown properties, backups, validation, online mode, and EULA settings.
- File manager, text editor, player list, whitelist/OP/ban/kick/say tools, and connection history.

For the first launch, create a profile, choose a server type and Minecraft version, download the server software, and accept the Minecraft EULA yourself. MineHost does not accept the EULA for you. The server binds to `127.0.0.1` by default; LAN access and external connections require network configuration.

Forge and NeoForge are prepared by their official installers using a Java runtime, with a separate consent step. This installs Minecraft server software into the profile; it does not install MineHost. Fabric can be installed from the app. A compatible runtime is required for the selected Minecraft version. Vanilla does not require a separate loader installation.

## Mods and plugins in the app

Paper/Purpur profiles show a “Plugins” tab; Fabric/Forge/NeoForge profiles show “Mods” and “Modpacks”. Vanilla hides incompatible tabs. The backend checks the profile’s server type and avoids loading incompatible JAR files. Mod and plugin files are kept in the appropriate server directories, such as `plugins`, `mods`, or `config`.

The interface supports uploading one or more JAR files from your computer, listing and searching installed files, enabling or disabling supported entries, and removing files. MineHost makes a backup in `.minehost/addon-backups` before destructive changes where applicable.

The built-in Modrinth catalog includes categories, descriptions, galleries, versions, and required dependencies. CurseForge integration is prepared through its API; API availability and authorization requirements may apply. See [CurseForge notes](docs/CURSEFORGE.md).

Compatible `.mrpack` files can be imported with checks for archive size, hashes, dependencies, and file paths. CurseForge ZIP archives, Quilt, and arbitrary modpack formats are not supported. Modpack imports do not bypass the source platform’s requirements or license terms.

## Your own world

Go to “Worlds” → “Upload a world”, select a folder containing `level.dat` or a ZIP archive, enter a name, and upload it. The source is copied; it is not moved or deleted.

The source should be saved while Minecraft is stopped, and the world version must be compatible with the profile. Limits: 32 GiB and 100,000 files. ZIP files are checked for unsafe paths. The selected world is installed as the profile’s active world; existing world data is protected as part of the operation.

## Internet access and playit.gg

In the playit.gg tab, select “Set up playit and get an address”. MineHost verifies and downloads the official standalone agent, then starts it for the app. The first sign-in and account approval are handled by the owner. One agent can serve all tunnels assigned to it.

The “Connection” section also provides LAN/external diagnostics, A/AAAA/SRV record checks, WAN/public IPv4 comparison to help identify CGNAT, firewall/UAC checks, and UPnP information. These checks are diagnostic: MineHost does not change router settings or open firewall ports automatically. Review any network changes before applying them. See [playit automatic setup](docs/PLAYIT_AUTO.md).

## Backups and restore

Full ZIP backups are created after Java has stopped. For a running server, MineHost offers coordinated maintenance: it runs `stop`, waits for the server to save and exit, creates the ZIP, then restarts the server. If the operation fails, the app reports the failure and does not treat an incomplete archive as a successful backup. A force-kill is available only as a last resort.

Full-server or world-only restore is available with a protective backup. You can configure a local schedule and retention policy, or use Google Drive with Desktop OAuth PKCE, `drive.file`, Windows Credential Manager, and resumable uploads. MineHost stores the OAuth credentials locally; access and removal are managed by you. See [Google Drive](docs/GOOGLE_DRIVE.md).

## Permanent deletion

“Delete server” opens a warning and requires the exact server name and explicit confirmation that the action is irreversible. The server’s files and associated local data are removed, including its UUID-specific data and local backups.

Other profiles and shared Java runtimes/cache are retained. Google Drive cloud backups, account tunnels, and external network settings are not deleted by this action. Review the confirmation carefully before proceeding.

## Data and builds

Data is stored under `%LOCALAPPDATA%\dev.frish07.minehost\`, including `servers/<UUID>`, `backups`, `cache`, `config/minehost.sqlite`, and `runtimes`. The app does not run servers in the background after it exits.

Builds run exclusively on GitHub Actions `windows-latest` with pinned dependencies, lockfiles, and Cargo/npm caches. Rust formatting, Clippy, tests, TypeScript, frontend, backend, NSIS, and packaging checks run in CI. Setup, EXE, and SHA-256 checksums are published with releases. See [cloud build](docs/CLOUD_BUILD.md).

UI verification uses real Chromium with mocked Tauri commands and saves PNGs to Actions Artifacts. It does not replace testing against a real Minecraft server. See the [1.0 release notes](docs/RELEASE_1_0.md), [roadmap](docs/ROADMAP.md), and [privacy policy](PRIVACY.md).

MineHost is not affiliated with Mojang, Microsoft, Aternos, PaperMC, or Eclipse Adoptium.

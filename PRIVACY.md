# Privacy policy

MineHost stores server profiles, worlds, logs, settings, backups and managed Java runtimes on the user's computer. MineHost has no analytics, advertising, telemetry endpoint, developer account login or automatic upload of these files.

Network access initiated by MineHost follows explicit user actions:

- Loading Minecraft versions and installing Paper contact the official PaperMC download service. The service receives the requested version/download URL, IP address and the MineHost User-Agent.
- Selecting Vanilla, Purpur or Fabric contacts Mojang's official manifests/downloads, the Purpur API, or Fabric Meta respectively. Installing Fabric also allows its launcher, after the user starts Java, to fetch official libraries and Minecraft files.
- Searching Modrinth sends the entered search text, loader/Minecraft filters, sort order and page offset to its public API. Opening versions and installing addons sends project/version requests and downloads JARs from the official Modrinth CDN. The service receives network metadata and the MineHost User-Agent. Installed files and server logs are not uploaded to Modrinth. See [Modrinth privacy](https://modrinth.com/legal/privacy).
- Downloading Java, after consent, contacts the Adoptium API and its GitHub-hosted Temurin releases. These providers receive the request and network metadata. Merely detecting an installed Java runtime is local.
- Starting a Minecraft server launches a separate Java process. Minecraft/Paper may contact their authentication or dependency services; third-party plugins may make their own requests. MineHost does not sandbox plugin network access. The user chooses whether to run these programs.

MineHost does not currently implement Google Drive, external tunnels or cloud backup uploads. Windows WebView2 and Windows security/update services are Microsoft components with their own privacy behavior.

The file/world managers operate locally. World archives are not uploaded. The player manager reads Minecraft's local lists and sends administrative commands to the local Java process. Minecraft may contact its account services when resolving player names. The online list is a manual snapshot, not telemetry; MineHost does not send player lists to its developer.

Provider information: [PaperMC](https://papermc.io/), [Eclipse Foundation privacy](https://www.eclipse.org/legal/privacy.php), [GitHub privacy](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement), [Microsoft privacy](https://privacy.microsoft.com/privacystatement). No API-specific privacy-policy URL for the PaperMC download service has been verified; the MineHost request behavior is described above.

The optional code-signing pipeline transfers public build artifacts and build provenance from GitHub Actions to SignPath. This happens on GitHub runners; no end-user server directories, logs or personal Minecraft data are submitted. See [SignPath privacy](https://signpath.io/privacy-policy).

In 0.4.0 Forge/NeoForge metadata and installer downloads contact their official Maven servers. With explicit consent MineHost runs the verified installer in a separate Java process; that installer contacts its official dependency sources. Local .mrpack imports download approved files from the Modrinth CDN, GitHub/raw.githubusercontent.com or GitLab using hashes recorded in the pack. Gallery images are fetched from the Modrinth CDN when a project is opened. Connection events observed in local server logs are retained locally in SQLite, up to 10,000 events per profile, and are not uploaded. Dimension transfers and backup restoration remain local.

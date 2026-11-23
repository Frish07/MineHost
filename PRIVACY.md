# Privacy policy

MineHost stores server profiles, worlds, logs, settings, backups and managed Java runtimes on the user's computer. MineHost has no analytics, advertising, telemetry endpoint, developer account login or automatic upload of these files.

Network access initiated by MineHost follows explicit user actions:

- Loading Minecraft versions and installing Paper contact the official PaperMC download service. The service receives the requested version/download URL, IP address and the MineHost User-Agent.
- Downloading Java, after consent, contacts the Adoptium API and its GitHub-hosted Temurin releases. These providers receive the request and network metadata. Merely detecting an installed Java runtime is local.
- Starting a Minecraft server launches a separate Java process. Minecraft/Paper may contact their authentication or dependency services; third-party plugins may make their own requests. MineHost does not sandbox plugin network access. The user chooses whether to run these programs.

MineHost does not currently implement Google Drive, external tunnels or cloud backup uploads. Windows WebView2 and Windows security/update services are Microsoft components with their own privacy behavior.

Provider information: [PaperMC](https://papermc.io/), [Eclipse Foundation privacy](https://www.eclipse.org/legal/privacy.php), [GitHub privacy](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement), [Microsoft privacy](https://privacy.microsoft.com/privacystatement). No API-specific privacy-policy URL for the PaperMC download service has been verified; the MineHost request behavior is described above.

The optional code-signing pipeline transfers public build artifacts and build provenance from GitHub Actions to SignPath. This happens on GitHub runners; no end-user server directories, logs or personal Minecraft data are submitted. See [SignPath privacy](https://signpath.io/privacy-policy).

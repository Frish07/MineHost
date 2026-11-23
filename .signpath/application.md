# Draft application — MineHost

Owner to submit and confirm; this file is not an application submission or a declaration that enrollment requirements have been verified.

- Project: MineHost
- Repository: https://github.com/Frish07/MineHost
- Download page: https://github.com/Frish07/MineHost/releases
- License: MIT — https://github.com/Frish07/MineHost/blob/main/LICENSE
- Maintainer and proposed signing approver: Frish07
- Code signing policy: https://github.com/Frish07/MineHost/blob/main/CODE_SIGNING_POLICY.md
- Privacy policy: https://github.com/Frish07/MineHost/blob/main/PRIVACY.md
- Owner contact email: supply privately in the application form.

## Description

MineHost is a free, open-source Windows desktop manager for local Minecraft Paper servers. It is built with Rust, Tauri, React and SQLite. Users create independent server profiles, download official Paper builds with SHA-256 verification, accept the Minecraft EULA themselves, detect Java or explicitly authorize a private Temurin JRE download, and start/stop Java server processes. It includes a live console, local plugin import with a code-execution warning, properties editing and backups of stopped servers. MineHost does not disable Windows security or change firewall/router settings.

All application builds run on GitHub-hosted windows-latest runners from public source with committed Cargo/npm lockfiles. We request signing for minehost.exe and the Windows x64 NSIS setup executable. The EXE is signed before packaging, and the setup executable afterward; both are verified before release. The generated NSIS uninstaller is not independently signed in this initial integration. No Minecraft, Java or third-party plugin binaries are submitted for re-signing.

The project is new and currently has unsigned pre-releases. Its initial code was developed with AI assistance; the owner must review the source and release scripts and verify the relevant account/security requirements before applying. Please assess eligibility rather than assume any established project reputation.

## Owner confirmations still required

Account access/MFA, acceptance of provider terms, owner review of source/build scripts, production policy/approver configuration, and consent to installing the SignPath GitHub App for this repository. Do not assert these are completed until the owner has verified them.

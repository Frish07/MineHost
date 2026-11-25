# Code signing policy

**Status: signing postponed indefinitely by the project owner; it may be dropped entirely.** Current releases are unsigned. The repository retains an optional, inactive SignPath integration; it does not establish a certificate, approval or SmartScreen reputation. Enrollment is not a required future milestone and will not be started without a new owner decision.

If the owner later chooses SignPath and is accepted, attribution would identify [SignPath.io](https://signpath.io) and [SignPath Foundation](https://signpath.org). No sponsorship or future enrollment is currently claimed.

The project owner [Frish07](https://github.com/Frish07) is the maintainer, source reviewer and proposed signing approver. Enrollment requires the owner to verify MFA for GitHub and SignPath and accept the provider's terms. Every signing request must be manually approved by the owner in SignPath. Automated agents do not approve signing requests on the owner's behalf.

Only MineHost code built from this public repository on GitHub-hosted Windows runners may be submitted. Production signing is restricted to version tags. The application is signed before NSIS packaging; the installer is then signed separately. Signatures and timestamps are verified before publication, and SHA-256 checksums are calculated afterward. Third-party Java, Paper and Minecraft binaries are downloaded at runtime with the user's authorization and are not re-signed or included in the MineHost installer. The generated NSIS uninstaller is not independently signed by this workflow; this coverage must be disclosed to SignPath during review.

When `WINDOWS_SIGNING_ENABLED=true`, missing configuration, rejected approval, signing timeout or invalid signatures stop the release. There is no unsigned fallback in that mode. While signing is deferred, unsigned builds remain available and are explicitly identified as unsigned in release notes.

See the [privacy policy](PRIVACY.md) and [setup instructions](docs/CODE_SIGNING.md). A valid signature identifies the publisher and preserves integrity; it does not guarantee that Microsoft SmartScreen immediately suppresses warnings for a new release.

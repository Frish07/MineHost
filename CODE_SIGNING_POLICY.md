# Code signing policy

**Status: enrollment not completed.** Releases v0.1.0 and v0.1.1 are unsigned. The repository contains a prepared SignPath integration; that does not establish a certificate, approval or SmartScreen reputation.

Requested provider after acceptance: “Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).” This attribution describes the planned arrangement, not an existing sponsorship.

The project owner [Frish07](https://github.com/Frish07) is the maintainer, source reviewer and proposed signing approver. Enrollment requires the owner to verify MFA for GitHub and SignPath and accept the provider's terms. Every signing request must be manually approved by the owner in SignPath. Automated agents do not approve signing requests on the owner's behalf.

Only MineHost code built from this public repository on GitHub-hosted Windows runners may be submitted. Production signing is restricted to version tags. The application is signed before NSIS packaging; the installer is then signed separately. Signatures and timestamps are verified before publication, and SHA-256 checksums are calculated afterward. Third-party Java, Paper and Minecraft binaries are downloaded at runtime with the user's authorization and are not re-signed or included in the MineHost installer. The generated NSIS uninstaller is not independently signed by this workflow; this coverage must be disclosed to SignPath during review.

When `WINDOWS_SIGNING_ENABLED=true`, missing configuration, rejected approval, signing timeout or invalid signatures stop the release. There is no unsigned fallback in that mode. While enrollment is pending, unsigned builds remain available and are explicitly identified as unsigned in new release notes.

See the [privacy policy](PRIVACY.md) and [setup instructions](docs/CODE_SIGNING.md). A valid signature identifies the publisher and preserves integrity; it does not guarantee that Microsoft SmartScreen immediately suppresses warnings for a new release.

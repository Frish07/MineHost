param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'
$file = Get-Item -LiteralPath $Path
if ($file.PSIsContainer -or $file.Extension -ine '.exe') { throw 'Expected an EXE file' }
$signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
if ($signature.Status -ne 'Valid' -or !$signature.SignerCertificate) {
    throw "Trusted Authenticode signature required for $($file.Name): $($signature.Status)"
}
if (!$signature.TimeStamperCertificate) { throw "Trusted timestamp missing: $($file.Name)" }
Write-Output "Verified $($file.Name): $($signature.SignerCertificate.Subject)"

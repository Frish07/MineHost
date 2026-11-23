param(
    [Parameter(Mandatory)][string]$Path,
    [switch]$WriteGithubOutputs
)
$ErrorActionPreference = 'Stop'
$version = (Get-Content (Join-Path $PSScriptRoot '../package.json') -Raw | ConvertFrom-Json).version
if ($version -notmatch '^\d+\.\d+\.\d+([+-][0-9A-Za-z.-]+)?$') { throw 'Invalid package version' }
$expected = [Version](($version -split '[-+]')[0])
$info = (Get-Item -LiteralPath $Path).VersionInfo
if ($info.ProductName -cne 'MineHost') { throw "Unexpected PE product name: $($info.ProductName)" }
if ($info.FileMajorPart -ne $expected.Major -or $info.FileMinorPart -ne $expected.Minor -or $info.FileBuildPart -ne $expected.Build) {
    throw 'PE file version does not match package.json'
}
$productVersion = [Version](($info.ProductVersion -split '[-+]')[0])
if ($productVersion.Major -ne $expected.Major -or $productVersion.Minor -ne $expected.Minor -or $productVersion.Build -ne $expected.Build) {
    throw 'PE product version does not match package.json'
}
$metadata = [ordered]@{
    'version' = $version
    'product-version' = $info.ProductVersion
    'file-version' = $info.FileVersion
}
foreach ($entry in $metadata.GetEnumerator()) {
    if (!$entry.Value -or $entry.Value -notmatch '^[0-9A-Za-z.+-]+$') { throw "Invalid PE metadata: $($entry.Key)" }
    if ($WriteGithubOutputs) {
        if (!$env:GITHUB_OUTPUT) { throw 'GITHUB_OUTPUT is required on the Actions runner' }
        "$($entry.Key)=$($entry.Value)" | Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding utf8
    }
}
Write-Output "Metadata verified: $([IO.Path]::GetFileName($Path)), MineHost $version, ProductVersion=$($info.ProductVersion), FileVersion=$($info.FileVersion)"

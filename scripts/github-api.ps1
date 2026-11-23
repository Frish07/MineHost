param(
    [ValidateSet('GET','POST')][string]$Method = 'GET',
    [Parameter(Mandatory)][string]$Route,
    [string]$Body,
    [switch]$RawText
)
# Uses existing Git Credential Manager; installs nothing and never prints credentials.
$ErrorActionPreference = 'Stop'
if ($Route -notmatch '^repos/Frish07/MineHost(/[A-Za-z0-9_./?=&%-]*)?$') { throw 'Only the MineHost repository is allowed' }
$credentialText = "protocol=https`nhost=github.com`n`n" | git credential fill
if ($LASTEXITCODE -ne 0) { throw 'GitHub authentication is unavailable' }
$token = ($credentialText | Where-Object { $_ -like 'password=*' } | Select-Object -First 1) -replace '^password=', ''
if (!$token) { throw 'Existing GitHub credential not found' }
$headers = @{Authorization="Bearer $token";Accept='application/vnd.github+json';'X-GitHub-Api-Version'='2022-11-28';'User-Agent'='MineHost-cloud-build'}
$parameters = @{Uri="https://api.github.com/$Route";Headers=$headers;Method=$Method}
if ($Body) { $parameters.Body = $Body; $parameters.ContentType='application/json' }
try {
    $response = Invoke-RestMethod @parameters
    if ($RawText) { $response } else { $response | ConvertTo-Json -Depth 30 }
}
finally { $token=$null; $credentialText=$null; $headers=$null }

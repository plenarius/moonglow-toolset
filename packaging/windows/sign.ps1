# Signs the files given (Authenticode, Azure Artifact Signing) with jsign:
#
#   packaging\windows\sign.ps1 target\dist\moonglow.exe target\dist\mg.exe
#
# build-installer.ps1 -Sign runs it, and Inno Setup runs it for the
# installer and its uninstaller. The access token is $env:MOONGLOW_SIGN_TOKEN;
# without one it signs in to Azure with GitHub Actions' OIDC token
# (AZURE_CLIENT_ID, AZURE_TENANT_ID; the job needs id-token: write), so no
# signing secret is stored anywhere. Outside Actions, set the token from
# `az account get-access-token --resource https://codesigning.azure.net`.
param([Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Files)
$ErrorActionPreference = "Stop"

$jsignVersion = "7.6"
$jsignSha256 = "6cf58aa080e7ff82656f320cff8d4a3bff30408df8766806dcba3740dd839fa5"
$jsign = Join-Path $PSScriptRoot "..\..\target\jsign\jsign-$jsignVersion.jar"
if (-not (Test-Path $jsign)) {
    New-Item -ItemType Directory -Force -Path (Split-Path $jsign) | Out-Null
    Invoke-WebRequest -UseBasicParsing -OutFile "$jsign.part" `
        -Uri "https://github.com/ebourg/jsign/releases/download/$jsignVersion/jsign-$jsignVersion.jar"
    if ((Get-FileHash -Algorithm SHA256 "$jsign.part").Hash -ne $jsignSha256) {
        Remove-Item -Force "$jsign.part"
        throw "jsign-$jsignVersion.jar: wrong SHA-256"
    }
    Move-Item -Force "$jsign.part" $jsign
}

if (-not $env:MOONGLOW_SIGN_TOKEN) {
    if (-not $env:ACTIONS_ID_TOKEN_REQUEST_URL) {
        throw "No MOONGLOW_SIGN_TOKEN, and no GitHub OIDC token to sign in with"
    }
    $oidc = (Invoke-RestMethod -Headers @{ Authorization = "bearer $env:ACTIONS_ID_TOKEN_REQUEST_TOKEN" } `
        -Uri "$env:ACTIONS_ID_TOKEN_REQUEST_URL&audience=api://AzureADTokenExchange").value
    Write-Host "::add-mask::$oidc"
    az login --service-principal -u $env:AZURE_CLIENT_ID -t $env:AZURE_TENANT_ID `
        --federated-token $oidc --allow-no-subscriptions | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "az login failed" }
    $token = az account get-access-token --resource https://codesigning.azure.net --query accessToken -o tsv
    if ($LASTEXITCODE -ne 0 -or -not $token) { throw "No code signing access token" }
    Write-Host "::add-mask::$token"
    az logout
    # Kept for the rest of this process and what it starts (Inno Setup's
    # signing of the installer), so it signs in once.
    $env:MOONGLOW_SIGN_TOKEN = $token
}

java -jar $jsign --storetype TRUSTEDSIGNING --keystore eus.codesigning.azure.net `
    --storepass $env:MOONGLOW_SIGN_TOKEN --alias orthartifacts/orthca-public `
    --tsaurl http://timestamp.acs.microsoft.com --tsmode RFC3161 @Files
if ($LASTEXITCODE -ne 0) { throw "jsign failed" }

# What Windows itself sees: a valid signature, or the build stops.
foreach ($f in $Files) {
    $s = Get-AuthenticodeSignature $f
    if ($s.Status -ne "Valid") { throw "${f}: signature $($s.Status): $($s.StatusMessage)" }
    Write-Host "${f}: signed by $($s.SignerCertificate.Subject)"
}

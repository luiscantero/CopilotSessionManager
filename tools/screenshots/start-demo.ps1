<#
.SYNOPSIS
Opens VS Code with the extension and fictional demo sessions, for screenshots.

.DESCRIPTION
Generates demo data (make_demo_data.py) in a sandbox folder and starts VS Code with a separate
profile and extensions folder. COPILOT_HOME and APPDATA point the extension's backend at the
sandbox, so no real sessions are shown or changed. One demo session is marked "In use" while
VS Code is open. Close the VS Code window to end the demo.

.EXAMPLE
./tools/screenshots/start-demo.ps1
#>
param(
    [string]$Sandbox = (Join-Path ([IO.Path]::GetTempPath()) "copilot-session-manager-demo"),
    [string]$Vsix
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "../..")
if (-not $Vsix) {
    $Vsix = Get-ChildItem (Join-Path $repo "vscode-extension") -Filter "*.vsix" |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
    if (-not $Vsix) { throw "No VSIX found. Run 'npm run package' in vscode-extension first." }
}

$info = python (Join-Path $PSScriptRoot "make_demo_data.py") $Sandbox | ConvertFrom-Json
$userData = Join-Path $Sandbox "vscode-user"
$extensions = Join-Path $Sandbox "vscode-extensions"
New-Item -ItemType Directory -Force (Join-Path $userData "User") | Out-Null
@{
    "workbench.startupEditor"                       = "none"
    "workbench.tips.enabled"                        = $false
    "workbench.secondarySideBar.defaultVisibility"  = "hidden"
    "chat.disableAIFeatures"                        = $true
    "security.workspace.trust.enabled"              = $false
    "telemetry.telemetryLevel"                      = "off"
    "update.mode"                                   = "none"
    "extensions.autoUpdate"                         = $false
    "window.restoreWindows"                         = "none"
} | ConvertTo-Json | Set-Content (Join-Path $userData "User/settings.json") -Encoding utf8

$env:COPILOT_HOME = $info.copilotHome
$env:APPDATA = $info.appData
$env:LOCALAPPDATA = Join-Path $Sandbox "localappdata"

code --user-data-dir $userData --extensions-dir $extensions --install-extension $Vsix --force | Out-Null

# A running Copilot CLI keeps inuse.<pid>.hold open; holding it marks the session "In use".
$hold = [IO.File]::Open($info.inUseHold, "OpenOrCreate", "ReadWrite", "ReadWrite")
try {
    Write-Host "Demo VS Code is starting. Press Ctrl+Alt+F to search; close the window to end the demo."
    code --user-data-dir $userData --extensions-dir $extensions --new-window --wait (Join-Path $Sandbox "repos/storefront-web")
} finally {
    $hold.Dispose()
}

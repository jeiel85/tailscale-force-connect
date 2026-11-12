# tailscale-force-connect web installer
#
#   cmd:        powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"
#   PowerShell: irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex
#
# Skip the post-install disconnect test (e.g. when connected to this PC over Tailscale):
#   cmd:        set TFC_NOTEST=1 && powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"
#   PowerShell: $env:TFC_NOTEST=1; irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex
#
# Change how long a manual disconnect lasts before Tailscale reconnects (default 1h, 0 = do not set):
#   cmd:        set TFC_RECONNECT_AFTER=30m && powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"
#   PowerShell: $env:TFC_RECONNECT_AFTER='30m'; irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex
#
# Downloads the latest release zip from GitHub, extracts it to a temp folder,
# runs install.ps1 (which asks for administrator rights and installs the management window
# to Program Files first), opens that window, then deletes the temp folder.
# On failure it throws, so `powershell -c "..."` exits with a non-zero code.
# Keep this file ASCII-only: Invoke-RestMethod may not decode it as UTF-8.
# Do not use 'exit' here: the script runs inside the caller's session via iex.
function Invoke-TailscaleForceConnectInstall {
    $ErrorActionPreference = 'Stop'
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch { }

    $url = 'https://github.com/jeiel85/tailscale-force-connect/releases/latest/download/tailscale-force-connect.zip'
    $tmp = Join-Path $env:TEMP ('tailscale-force-connect-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $zip = Join-Path $tmp 'package.zip'
        Write-Host "Downloading $url"
        $oldProgress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'   # the progress bar makes Invoke-WebRequest very slow on PowerShell 5.1
        try { Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing } finally { $ProgressPreference = $oldProgress }
        Expand-Archive -Path $zip -DestinationPath $tmp

        $installer = Join-Path $tmp 'tailscale-force-connect\install.ps1'
        if (-not (Test-Path $installer)) { throw 'install.ps1 was not found in the downloaded package.' }

        $installArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $installer)
        if ($env:TFC_NOTEST -and $env:TFC_NOTEST.Trim() -ne '0') {
            $installArgs += '-NoTest'
            Write-Host 'TFC_NOTEST is set: the disconnect test will be skipped.'
        }
        if ($env:TFC_RECONNECT_AFTER) {
            $installArgs += @('-ReconnectAfter', $env:TFC_RECONNECT_AFTER.Trim())
            Write-Host "TFC_RECONNECT_AFTER is set: reconnect after $($env:TFC_RECONNECT_AFTER.Trim())."
        }
        Write-Host 'Running install.ps1 (a UAC prompt will appear)...'
        & powershell.exe @installArgs
        if ($LASTEXITCODE -ne 0) { throw "install.ps1 failed with exit code $LASTEXITCODE." }

        # Open the management window that install.ps1 put in Program Files (as the current user, not elevated).
        $gui = Join-Path $env:ProgramFiles 'Tailscale Force Connect\TailscaleForceConnect.exe'
        if (Test-Path -LiteralPath $gui) {
            Write-Host 'Opening Tailscale Force Connect (also in the Start menu).'
            Start-Process -FilePath $gui
        }
    } catch {
        Write-Error "tailscale-force-connect install failed: $($_.Exception.Message)"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}
Invoke-TailscaleForceConnectInstall

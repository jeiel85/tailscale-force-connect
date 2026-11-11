# tailscale-force-connect web installer
#
#   cmd:        powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"
#   PowerShell: irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex
#
# Downloads the latest release zip from GitHub, extracts it to a temp folder,
# runs install.ps1 (which asks for administrator rights), then deletes the temp folder.
# Keep this file ASCII-only: Invoke-RestMethod may not decode it as UTF-8.
# Do not use 'exit' here: the script runs inside the caller's session via iex.
& {
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
        if (-not (Test-Path $installer)) { throw "install.ps1 was not found in the downloaded package." }

        Write-Host 'Running install.ps1 (a UAC prompt will appear)...'
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "install.ps1 exited with code $LASTEXITCODE. See C:\ProgramData\TailscaleAutoConnect\install.log"
        }
    } catch {
        Write-Error "tailscale-force-connect install failed: $($_.Exception.Message)"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

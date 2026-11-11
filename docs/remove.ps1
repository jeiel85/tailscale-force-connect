# tailscale-force-connect web uninstaller
#
#   cmd:        powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/remove.ps1 | iex"
#   PowerShell: irm https://jeiel85.github.io/tailscale-force-connect/remove.ps1 | iex
#
# Runs the uninstall.ps1 that install.ps1 copied to C:\ProgramData\TailscaleAutoConnect\.
# If that copy is missing (very early installs), downloads the latest release zip and runs its uninstall.ps1.
# Removes the scheduled task, the install folder and the event log source. Tailscale itself is not touched.
# Keep this file ASCII-only and do not use 'exit' (it runs inside the caller's session via iex).
& {
    $ErrorActionPreference = 'Stop'
    $local = 'C:\ProgramData\TailscaleAutoConnect\uninstall.ps1'
    $tmp = $null
    try {
        if (Test-Path $local) {
            # Run it in place: the folder is admin-only, so nothing can swap the file before it runs elevated.
            # (A script may delete its own folder while running; PowerShell has already read it.)
            $uninstaller = $local
        } else {
            try {
                [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
            } catch { }
            $url = 'https://github.com/jeiel85/tailscale-force-connect/releases/latest/download/tailscale-force-connect.zip'
            $tmp = Join-Path $env:TEMP ('tailscale-force-connect-' + [guid]::NewGuid().ToString('N'))
            New-Item -ItemType Directory -Path $tmp | Out-Null
            $zip = Join-Path $tmp 'package.zip'
            Write-Host "Downloading $url"
            $oldProgress = $ProgressPreference
            $ProgressPreference = 'SilentlyContinue'
            try { Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing } finally { $ProgressPreference = $oldProgress }
            Expand-Archive -Path $zip -DestinationPath $tmp
            $uninstaller = Join-Path $tmp 'tailscale-force-connect\uninstall.ps1'
            if (-not (Test-Path $uninstaller)) { throw 'uninstall.ps1 was not found in the downloaded package.' }
        }

        Write-Host 'Running uninstall.ps1 (a UAC prompt will appear)...'
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $uninstaller
        if ($LASTEXITCODE -ne 0) { throw "uninstall.ps1 failed with exit code $LASTEXITCODE." }
    } catch {
        Write-Error "tailscale-force-connect uninstall failed: $($_.Exception.Message)"
    } finally {
        if ($tmp) { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
    }
}

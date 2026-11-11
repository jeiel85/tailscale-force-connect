# tailscale-force-connect web uninstaller
#
#   cmd:        powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/remove.ps1 | iex"
#   PowerShell: irm https://jeiel85.github.io/tailscale-force-connect/remove.ps1 | iex
#
# Runs the uninstall.ps1 that install.ps1 copied to C:\ProgramData\TailscaleAutoConnect\, but only if that
# folder and file are owned by and writable only by Administrators/SYSTEM. Otherwise (missing, or possibly
# planted by another user) it downloads the latest release zip and runs its uninstall.ps1.
# Removes the scheduled task, the install folder and the event log source. Tailscale itself is not touched.
# Keep this file ASCII-only and do not use 'exit' (it runs inside the caller's session via iex).
function Test-TailscaleForceConnectTrustedPath([string]$Path) {
    # True only if $Path is not a link, is owned by Administrators/SYSTEM, and only they can modify it.
    # Any user can create folders under C:\ProgramData, so existence alone proves nothing.
    $trusted = @('S-1-5-32-544', 'S-1-5-18')
    $writeMask = 0x2 -bor 0x4 -bor 0x10 -bor 0x40 -bor 0x100 -bor 0x10000 -bor 0x40000 -bor 0x80000
    try {
        $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $false }
        $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
        if ($trusted -notcontains $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value) { return $false }
        foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
            if ($rule.AccessControlType -ne 'Allow') { continue }
            if (([int]$rule.FileSystemRights -band $writeMask) -eq 0) { continue }
            if ($trusted -notcontains $rule.IdentityReference.Value) { return $false }
        }
        return $true
    } catch { return $false }
}

function Invoke-TailscaleForceConnectRemove {
    $ErrorActionPreference = 'Stop'
    $localDir = 'C:\ProgramData\TailscaleAutoConnect'
    $local = Join-Path $localDir 'uninstall.ps1'
    $tmp = $null
    try {
        $useLocal = $false
        if (Test-Path -LiteralPath $local) {
            if ((Test-TailscaleForceConnectTrustedPath $localDir) -and (Test-TailscaleForceConnectTrustedPath $local)) {
                $useLocal = $true
            } else {
                Write-Warning "$local is not protected (owner or permissions are not admin-only). Ignoring it and using the release package instead."
            }
        }
        if ($useLocal) {
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
Invoke-TailscaleForceConnectRemove

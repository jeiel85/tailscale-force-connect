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
# Change how long Tailscale may look connected but stay offline before its service is restarted (default 3m, 0 = off):
#   cmd:        set TFC_HEAL_AFTER=10m && powershell -c "irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex"
#   PowerShell: $env:TFC_HEAL_AFTER='10m'; irm https://jeiel85.github.io/tailscale-force-connect/get.ps1 | iex
#
# What it does:
#   1) Asks the GitHub API for the latest release and the SHA-256 that GitHub recorded for its zip, downloads that
#      exact file to a temp folder and checks the hash. It stops if the hash differs or is missing.
#   2) Asks for administrator rights (UAC). The elevated PowerShell copies the zip into a new folder that only
#      administrators can write to, checks the hash again, extracts it there and runs install.ps1 from that folder.
#      So nothing that another program of the same user could swap in the temp folder while the UAC prompt waits
#      is run as administrator. (The small elevated step itself is written to the temp folder, but the elevated
#      command line carries its SHA-256 and runs it only if it matches.)
#   3) Prints install.ps1's result here, opens the management window (installed to Program Files), deletes the temp folder.
# On failure it throws, so `powershell -c "..."` exits with a non-zero code.
# Keep this file ASCII-only: Invoke-RestMethod may not decode it as UTF-8.
# Do not use 'exit' here: the script runs inside the caller's session via iex.

# ---- shared with remove.ps1: keep this block identical in both files (tests\web-install.ps1 checks) ----

# The elevated step, run by a new Windows PowerShell. Template values: __SRC__ (downloaded zip), __SHA__ (its SHA-256),
# __SCRIPT__ (install.ps1 or uninstall.ps1), __ARGS__ (its arguments), __RESULT__ (file that receives its report).
function Get-TfcElevatedTemplate {
@'
$ErrorActionPreference = 'Stop'
# If this inherited PowerShell 7's module path, Windows PowerShell 5.1 cannot load cmdlets such as Get-FileHash.
$env:PSModulePath = "$env:ProgramFiles\WindowsPowerShell\Modules;$env:SystemRoot\system32\WindowsPowerShell\v1.0\Modules"
$src = __SRC__
$sha = __SHA__
$runName = __SCRIPT__
$runArgs = @(__ARGS__)
$resultFile = __RESULT__
$code = 1
$stage = $null
try {
    # A new folder that only Administrators and SYSTEM can write to. Users may add files to subfolders of
    # Windows\Temp, so the folder gets its own protected ACL when it is created (no window to slip files in).
    $admins = New-Object Security.Principal.SecurityIdentifier 'S-1-5-32-544'
    $system = New-Object Security.Principal.SecurityIdentifier 'S-1-5-18'
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetOwner($admins)
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in $system, $admins) {
        $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')))
    }
    $stage = Join-Path $env:SystemRoot ('Temp\tailscale-force-connect-' + [guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($stage, $acl)
    $item = Get-Item -LiteralPath $stage -Force
    $owner = (Get-Acl -LiteralPath $stage).GetOwner([Security.Principal.SecurityIdentifier]).Value
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or (@('S-1-5-32-544', 'S-1-5-18') -notcontains $owner) -or (Get-ChildItem -LiteralPath $stage -Force)) {
        throw "Could not create a private folder for the package: $stage"
    }

    $zip = Join-Path $stage 'package.zip'
    Copy-Item -LiteralPath $src -Destination $zip
    $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $sha) { throw "The package changed after it was downloaded (SHA-256 $actual, expected $sha). Nothing was run." }
    Write-Host "SHA-256 verified again in $stage"
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $stage)
    $path = Join-Path $stage ('tailscale-force-connect\' + $runName)
    if (-not (Test-Path -LiteralPath $path)) { throw "$runName was not found in the package." }

    # The script writes its report to $resultFile itself (it only creates a new file, never reuses one).
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $path @runArgs -ResultFile $resultFile
    $code = $LASTEXITCODE
} catch {
    $msg = $_.Exception.Message
    Write-Host $msg -ForegroundColor Red
    # The script did not run, so nothing else has created the result file yet.
    try {
        $fs = [IO.File]::Open($resultFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $writer = New-Object IO.StreamWriter($fs, (New-Object Text.UTF8Encoding $true))
        $writer.WriteLine($msg)
        $writer.Dispose()
    } catch { }
} finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
}
exit $code
'@
}

function ConvertTo-TfcLiteral([string]$Text) { "'" + $Text.Replace("'", "''") + "'" }

function Invoke-TfcRetry([scriptblock]$Action, [string]$What) {
    for ($i = 1; $i -le 3; $i++) {
        try { return & $Action } catch {
            if ($i -eq 3) { throw "$What failed 3 times: $($_.Exception.Message)" }
            Write-Host "$What failed, retrying in 5 seconds ($i/3): $($_.Exception.Message)"
            Start-Sleep -Seconds 5
        }
    }
}

# Downloads the latest release zip into $Dir and checks it against the SHA-256 GitHub recorded for it.
function Get-TfcLatestPackage([string]$Dir) {
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch { }
    $api = 'https://api.github.com/repos/jeiel85/tailscale-force-connect/releases/latest'
    $headers = @{ 'User-Agent' = 'tailscale-force-connect-installer'; 'Accept' = 'application/vnd.github+json' }
    $rel = Invoke-TfcRetry -What 'Asking GitHub for the latest release' -Action { Invoke-RestMethod -Uri $api -Headers $headers -UseBasicParsing -TimeoutSec 30 }
    $asset = @($rel.assets | Where-Object { $_.name -eq 'tailscale-force-connect.zip' })[0]
    if (-not $asset) { throw "The latest release ($($rel.tag_name)) has no tailscale-force-connect.zip." }
    if ("$($asset.digest)" -notmatch '^sha256:([0-9a-f]{64})$') { throw "GitHub has no SHA-256 for the $($rel.tag_name) package, so it cannot be verified." }
    $sha = $Matches[1]
    $zip = Join-Path $Dir 'package.zip'
    Write-Host "Downloading $($rel.tag_name): $($asset.browser_download_url)"
    $oldProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'   # the progress bar makes Invoke-WebRequest very slow on PowerShell 5.1
    try {
        Invoke-TfcRetry -What 'Download' -Action { Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing -TimeoutSec 300 } | Out-Null
    } finally { $ProgressPreference = $oldProgress }
    $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $sha) { throw "The download does not match the release (SHA-256 $actual, expected $sha)." }
    Write-Host "SHA-256 verified: $sha"
    [pscustomobject]@{ Tag = $rel.tag_name; Zip = $zip; Sha256 = $sha }
}

# Writes the elevated step for $RunName to $Dir and returns the -Command text that runs it only if its SHA-256 matches.
function New-TfcElevatedCommand([string]$Dir, [string]$Zip, [string]$Sha256, [string]$RunName, [string[]]$RunArgs, [string]$ResultFile) {
    $argText = (@($RunArgs) | ForEach-Object { ConvertTo-TfcLiteral $_ }) -join ', '
    $values = @{
        SRC = ConvertTo-TfcLiteral $Zip; SHA = ConvertTo-TfcLiteral $Sha256; SCRIPT = ConvertTo-TfcLiteral $RunName
        ARGS = $argText; RESULT = ConvertTo-TfcLiteral $ResultFile
    }
    # One pass, so a value that happens to contain a later token (e.g. __RESULT__) is never substituted again.
    $text = [regex]::Replace((Get-TfcElevatedTemplate), '__(SRC|SHA|SCRIPT|ARGS|RESULT)__', [Text.RegularExpressions.MatchEvaluator]{ param($m) $values[$m.Groups[1].Value] })
    $bytes = (New-Object Text.UTF8Encoding $false).GetBytes($text)
    $file = Join-Path $Dir 'elevated-step.ps1'
    [IO.File]::WriteAllBytes($file, $bytes)
    $hash = -join ([Security.Cryptography.SHA256]::Create().ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') })
    # Plain -Command text without double quotes inside, so it survives the command line as one argument.
    '$b = [IO.File]::ReadAllBytes(' + (ConvertTo-TfcLiteral $file) + '); ' +
    '$h = -join ([Security.Cryptography.SHA256]::Create().ComputeHash($b) | ForEach-Object { $_.ToString(''x2'') }); ' +
    'if ($h -ne ''' + $hash + ''') { Write-Host ''The setup step was changed on disk. Stopping.''; exit 1 }; ' +
    '& ([scriptblock]::Create([Text.Encoding]::UTF8.GetString($b)))'
}

# Runs the command elevated (UAC), waits, prints the report it left and returns the exit code.
function Invoke-TfcElevated([string]$Command, [string]$ResultFile, [string]$What) {
    Write-Host "Running $What (a UAC prompt will appear)..."
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList ('-NoProfile -ExecutionPolicy Bypass -Command "' + $Command + '"') -PassThru -Wait
    } catch {
        throw "Administrator rights were not granted, so nothing was changed. Run the command again and choose Yes in the UAC prompt."
    }
    if (Test-Path -LiteralPath $ResultFile) {
        Get-Content -LiteralPath $ResultFile -Encoding UTF8 | ForEach-Object { Write-Host $_ }
    } else {
        Write-Host 'The elevated window left no report.'
    }
    $p.ExitCode
}

# ---- end of shared block ----

function Invoke-TailscaleForceConnectInstall {
    $ErrorActionPreference = 'Stop'
    $tmp = Join-Path $env:TEMP ('tailscale-force-connect-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $pkg = Get-TfcLatestPackage -Dir $tmp

        $installArgs = @()
        if ($env:TFC_NOTEST -and $env:TFC_NOTEST.Trim() -ne '0') {
            $installArgs += '-NoTest'
            Write-Host 'TFC_NOTEST is set: the disconnect test will be skipped.'
        }
        if ($env:TFC_RECONNECT_AFTER) {
            $installArgs += @('-ReconnectAfter', $env:TFC_RECONNECT_AFTER.Trim())
            Write-Host "TFC_RECONNECT_AFTER is set: reconnect after $($env:TFC_RECONNECT_AFTER.Trim())."
        }
        if ($env:TFC_HEAL_AFTER) {
            $installArgs += @('-HealAfter', $env:TFC_HEAL_AFTER.Trim())
            Write-Host "TFC_HEAL_AFTER is set: restart a stuck Tailscale after $($env:TFC_HEAL_AFTER.Trim())."
        }

        $result = Join-Path $env:TEMP ('tailscale-force-connect-install-' + [guid]::NewGuid().ToString('N') + '.txt')
        $command = New-TfcElevatedCommand -Dir $tmp -Zip $pkg.Zip -Sha256 $pkg.Sha256 -RunName 'install.ps1' -RunArgs $installArgs -ResultFile $result
        try {
            $code = Invoke-TfcElevated -Command $command -ResultFile $result -What "install.ps1 $($pkg.Tag)"
        } finally {
            Remove-Item -LiteralPath $result -Force -ErrorAction SilentlyContinue
        }
        if ($code -ne 0) { throw "install.ps1 failed with exit code $code." }

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

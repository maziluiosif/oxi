# Installs the latest oxi release on Windows (x86_64).
#
#   irm https://maziluiosif.github.io/oxi/install.ps1 | iex
#
# Files fetched with Invoke-WebRequest carry no Mark-of-the-Web, so SmartScreen
# does not block the unsigned executable the way it does for a browser download.
#
# Environment overrides:
#   OXI_VERSION      release tag to install (e.g. v1.8.0); defaults to the latest
#   OXI_INSTALL_DIR  install directory; defaults to %LOCALAPPDATA%\Programs\oxi

# Everything runs inside a function so a truncated download cannot execute half a script.
function Install-Oxi {
    $ErrorActionPreference = 'Stop'
    # Invoke-WebRequest's progress bar slows downloads down dramatically on Windows PowerShell 5.1.
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'maziluiosif/oxi'
    $asset = 'oxi-windows-x86_64.zip'

    # x64 builds also run on Windows on Arm through emulation.
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw "oxi only ships a 64-bit Windows build; build from source: https://github.com/$repo#build-and-run-from-source"
    }

    $version = $env:OXI_VERSION
    if ($version) {
        if (-not $version.StartsWith('v')) { $version = "v$version" }
        $base = "https://github.com/$repo/releases/download/$version"
    } else {
        $base = "https://github.com/$repo/releases/latest/download"
    }

    $installDir = if ($env:OXI_INSTALL_DIR) { $env:OXI_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\oxi' }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("oxi-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "oxi: downloading $asset$(if ($version) { " ($version)" })"
        $zip = Join-Path $tmp $asset
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $zip
        $sums = Join-Path $tmp 'SHA256SUMS'
        Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums

        $expected = $null
        foreach ($line in Get-Content $sums) {
            $parts = $line -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) { $expected = $parts[0] }
        }
        if (-not $expected) { throw "$asset is missing from SHA256SUMS" }
        $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash
        if ($actual -ne $expected) { throw "checksum mismatch for $asset (expected $expected, got $actual)" }
        Write-Host 'oxi: checksum verified'

        $extract = Join-Path $tmp 'extract'
        Expand-Archive -Path $zip -DestinationPath $extract -Force

        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        $exe = Join-Path $installDir 'oxi.exe'
        try {
            Copy-Item (Join-Path $extract 'oxi.exe') $exe -Force
        } catch {
            throw "could not replace $exe; close oxi and run the installer again"
        }
        # Not needed for this download, but clears the flag if an earlier browser install left one.
        Unblock-File $exe -ErrorAction SilentlyContinue
        Write-Host "oxi: installed $exe"

        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @($userPath -split ';' | Where-Object { $_ })
        if ($entries -notcontains $installDir) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $installDir) -join ';'), 'User')
            Write-Host "oxi: added $installDir to your user PATH (open a new terminal to use it)"
        }
        if (($env:Path -split ';') -notcontains $installDir) { $env:Path = "$env:Path;$installDir" }

        $programs = [Environment]::GetFolderPath('Programs')
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut((Join-Path $programs 'oxi.lnk'))
        $shortcut.TargetPath = $exe
        $shortcut.WorkingDirectory = $env:USERPROFILE
        $shortcut.Save()
        Write-Host 'oxi: added oxi to the Start menu'

        Write-Host 'oxi: done. Launch it from the Start menu, or run `oxi` in a project directory.'
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

Install-Oxi

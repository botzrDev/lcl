# Installs lcl from a GitHub release, on Windows, in PowerShell:
#
#   irm https://raw.githubusercontent.com/botzrDev/lcl/main/install.ps1 | iex
#
# It downloads the x86_64 Windows archive with the release's SHA256SUMS,
# and checks the archive against them. It then runs the lcl.exe in it,
# which must print the release's version, and only then puts it in place.
# Anything that fails before that installs nothing and stops with an
# error; run as a file (powershell -File install.ps1), it exits 1.
#
# Settings, all optional, from the environment:
#
#   LCL_VERSION         The release to install: v0.3.0-alpha.2 or
#                       0.3.0-alpha.2. Without it, the newest release.
#                       Until 1.0 that can be a pre-release.
#   LCL_INSTALL_DIR     Where lcl.exe goes. Without it,
#                       %LOCALAPPDATA%\Programs\lcl.
#   LCL_NO_MODIFY_PATH  If set, the install directory is not added to your
#                       user PATH. Without it, it is, if it is not there.
#   LCL_RELEASES_URL    Where the release files are, as URL/TAG/FILE.
#                       Without it, https://github.com/botzrDev/lcl/releases/download.
#   GITHUB_TOKEN        If set, sent to GitHub's API when it is asked for
#                       the newest release, for a higher rate limit.
#
# The work is one script block, so nothing it names is left in the
# session that ran it, and a script cut short in the download runs nothing.

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 may not offer TLS 1.2 by itself; GitHub needs it.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'botzrDev/lcl'
    $target = 'x86_64-pc-windows-msvc'

    function Say([string] $message) {
        [Console]::Error.WriteLine("lcl install: $message")
    }

    function Fail([string] $message) {
        throw "lcl install: $message"
    }

    if (-not [Environment]::Is64BitOperatingSystem) {
        Fail "there is no lcl build for 32-bit Windows; build it from source: https://github.com/$repo#source"
    }

    $tag = $env:LCL_VERSION
    if (-not $tag) {
        $headers = @{ Accept = 'application/vnd.github+json' }
        if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
        try {
            $releases = Invoke-RestMethod -UseBasicParsing -Headers $headers -Uri "https://api.github.com/repos/$repo/releases?per_page=1"
        } catch {
            Fail "can't ask GitHub for the newest release of lcl; set LCL_VERSION to choose one: $($_.Exception.Message)"
        }
        $tag = ($releases | Select-Object -First 1).tag_name
        if (-not $tag) { Fail 'GitHub lists no release of lcl' }
    }
    if (-not $tag.StartsWith('v')) { $tag = "v$tag" }
    $version = $tag.Substring(1)
    $base = if ($env:LCL_RELEASES_URL) { $env:LCL_RELEASES_URL } else { "https://github.com/$repo/releases/download" }
    $name = "lcl-$version-$target"
    $archive = "$name.zip"
    $dir = if ($env:LCL_INSTALL_DIR) { $env:LCL_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\lcl' }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("lcl-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Say "downloading lcl $version for $target"
        foreach ($file in @($archive, 'SHA256SUMS')) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$base/$tag/$file" -OutFile (Join-Path $tmp $file)
            } catch {
                Fail "can't download $base/$tag/${file}: $($_.Exception.Message); nothing was installed"
            }
        }

        $want = $null
        foreach ($line in Get-Content -LiteralPath (Join-Path $tmp 'SHA256SUMS')) {
            $fields = $line -split '\s+', 2
            if ($fields.Count -eq 2 -and ($fields[1] -eq $archive -or $fields[1] -eq "*$archive")) {
                $want = $fields[0].ToLowerInvariant()
                break
            }
        }
        if (-not $want) { Fail "SHA256SUMS of $tag has no line for $archive; nothing was installed" }
        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $tmp $archive)).Hash.ToLowerInvariant()
        if ($got -ne $want) {
            Fail "$archive does not match SHA256SUMS: its SHA-256 is $got, and SHA256SUMS says $want; nothing was installed"
        }

        try {
            Expand-Archive -LiteralPath (Join-Path $tmp $archive) -DestinationPath $tmp
        } catch {
            Fail "can't unpack ${archive}: $($_.Exception.Message); nothing was installed"
        }
        $exe = Join-Path $tmp "$name\lcl.exe"
        if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { Fail "$archive holds no $name\lcl.exe; nothing was installed" }
        try {
            $printed = (& $exe --version | Out-String).Trim()
        } catch {
            $printed = $_.Exception.Message
        }
        if ($printed -ne "lcl $version") {
            Fail "the lcl.exe in $archive does not run here as lcl $version; it printed: $printed; nothing was installed"
        }

        try {
            New-Item -ItemType Directory -Force -Path $dir | Out-Null
        } catch {
            Fail "can't make ${dir}: $($_.Exception.Message); nothing was installed"
        }
        $dest = Join-Path $dir 'lcl.exe'
        if (Test-Path -LiteralPath $dest -PathType Container) { Fail "$dest is a directory; nothing was installed" }
        $part = Join-Path $dir ".lcl.install.$PID.exe"
        try {
            Copy-Item -LiteralPath $exe -Destination $part
            Move-Item -LiteralPath $part -Destination $dest -Force
        } catch {
            Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue
            Fail "can't write ${dest}: $($_.Exception.Message); nothing was installed"
        }
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    Say "installed lcl $version as $dest"
    # Whether a PATH value, entries separated by ;, names $dir.
    function Test-Names([string] $path) {
        @($path -split ';' | Where-Object { $_ -and [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -eq $dir.TrimEnd('\') }).Count -gt 0
    }
    if ($env:LCL_NO_MODIFY_PATH) {
        if (-not (Test-Names $env:Path)) { Say "$dir is not on your PATH; add it to use lcl by name" }
    } else {
        # The user PATH as stored, so %VARIABLES% in it stay unexpanded.
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        try {
            $old = [string] $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $added = -not (Test-Names $old)
            if ($added) {
                $new = if ($old) { $old.TrimEnd(';') + ";$dir" } else { $dir }
                $key.SetValue('Path', $new, [Microsoft.Win32.RegistryValueKind]::ExpandString)
            }
        } finally {
            $key.Close()
        }
        if ($added) {
            # Setting a user variable tells running programs, such as
            # Explorer, that the environment changed, so new terminals see
            # the new PATH.
            [Environment]::SetEnvironmentVariable('LCL_INSTALL_PATH_CHANGED', '1', 'User')
            [Environment]::SetEnvironmentVariable('LCL_INSTALL_PATH_CHANGED', $null, 'User')
            Say "added $dir to your user PATH; terminals opened before now don't have it"
        }
        if (-not (Test-Names $env:Path)) { $env:Path = $env:Path.TrimEnd(';') + ";$dir" }
    }
    Say "the agent skill for this version: $base/$tag/SKILL.md"
}

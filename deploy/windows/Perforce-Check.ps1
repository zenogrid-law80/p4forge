#Requires -Version 5.1
# Shared preflight: do not inherit another workspace's Perforce settings.
function Protect-P4Diagnostic([string]$Text) {
    $text = [regex]::Replace($Text, '(?i)\b[0-9a-f]{32}\b', '[REDACTED]')
    $text = [regex]::Replace($text, '(?<![A-Za-z0-9_-])[A-Za-z0-9_-]{43}(?![A-Za-z0-9_-])', '[REDACTED]')
    if ($text.Length -gt 16384) { $text = $text.Substring(0, 16384) + '[truncated]' }
    return $text.Trim()
}
function Invoke-P4Check {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string]$Tickets,
        [string]$Port = 'p4forge.zenogrid.co.kr:1666',
        [string]$User = 'p4forge',
        [ValidateSet('utf8', 'none')][string]$Charset = 'utf8',
        [ValidateSet('version', 'login')][string]$Operation = 'login'
    )
    if ($Port -notmatch '^[A-Za-z0-9.:-]+$' -or $User -notmatch '^[A-Za-z0-9_.-]+$') { throw 'Invalid Perforce connection.' }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = (Resolve-Path -LiteralPath $Executable).Path
    $info.WorkingDirectory = [IO.Path]::GetPathRoot($info.FileName)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $info.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $info.EnvironmentVariables.Clear()
    foreach ($key in @('SystemRoot', 'WINDIR', 'PATH', 'TEMP', 'TMP')) {
        $value = [Environment]::GetEnvironmentVariable($key, 'Process')
        if ($null -ne $value) { $info.EnvironmentVariables[$key] = $value }
    }
    $info.EnvironmentVariables['P4TICKETS'] = (Resolve-Path -LiteralPath $Tickets).Path
    $info.EnvironmentVariables['P4CONFIG'] = ''
    $info.EnvironmentVariables['P4IGNORE'] = ''
    $info.EnvironmentVariables['P4ENVIRO'] = $(if ([Environment]::OSVersion.Platform -eq 'Win32NT') { 'NUL' } else { '/dev/null' })
    # Explicit UTF-8 matches the Unicode server and preserves Korean file names.
    $info.Arguments = $(if ($Operation -eq 'version') { '-V' } else { "-ztag -p $Port -u $User -C $Charset login -s" })
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    try {
        if (-not $process.Start()) { throw 'Could not start p4.exe.' }
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(15000)) {
            $process.Kill()
            $process.WaitForExit()
            throw 'Perforce preflight timed out after 15 seconds.'
        }
        return [pscustomobject]@{
            exit_code = $process.ExitCode
            stdout = Protect-P4Diagnostic ($stdout.GetAwaiter().GetResult())
            stderr = Protect-P4Diagnostic ($stderr.GetAwaiter().GetResult())
        }
    } finally { $process.Dispose() }
}

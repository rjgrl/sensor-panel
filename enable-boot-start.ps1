<#
.SYNOPSIS
    Manage DS916 display startup before Windows sign-in.
.DESCRIPTION
    Run Enable as administrator using your normal Windows account, after build.bat.
    The task runs with limited privileges using S4U (no password stored).
    Disable returns to sign-in-only startup. Stop is used before a rebuild;
    Remove is used by the uninstaller. Local, unencrypted themes are required.
#>
param(
    [ValidateSet('Enable', 'Disable', 'Stop', 'Remove')]
    [string]$Action = 'Enable'
)

$ErrorActionPreference = 'Stop'
$taskName = 'DS916 Tray Boot'
$taskApp = Join-Path $env:APPDATA 'DS916Tray\DS916Tray.exe'
$taskExisting = Get-ScheduledTask -TaskPath '\' -ErrorAction Stop | Where-Object TaskName -eq $taskName
if ($taskExisting -and (@($taskExisting.Actions).Count -ne 1 -or
    $taskExisting.Actions[0].Execute -ne $taskApp -or $taskExisting.Actions[0].Arguments -ne '--boot')) {
    throw "An unexpected task already uses the name '$taskName'; it was not changed."
}

if ($Action -ne 'Enable') {
    if ($taskExisting) {
        if ($Action -eq 'Disable') {
            Disable-ScheduledTask -InputObject $taskExisting | Out-Null
        }
        Stop-ScheduledTask -InputObject $taskExisting
        if ($Action -eq 'Remove') {
            Unregister-ScheduledTask -InputObject $taskExisting -Confirm:$false
        }
    }
    return
}

$taskIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$taskPrincipal = [Security.Principal.WindowsPrincipal]$taskIdentity
if (-not $taskPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script as administrator using your normal Windows account.'
}
if (-not (Test-Path -LiteralPath $taskApp -PathType Leaf)) {
    throw 'Build and install the updated app with build.bat first.'
}
$taskConfigPath = Join-Path (Split-Path -Parent $taskApp) 'config.json'
$taskConfig = Get-Content -LiteralPath $taskConfigPath -Raw | ConvertFrom-Json
if (-not $taskConfig.theme_path -or -not (Test-Path -LiteralPath $taskConfig.theme_path -PathType Leaf)) {
    throw 'Select and save a local theme in DS916 Settings first.'
}
if ($taskConfig.autostart -eq $false) {
    throw 'Enable Start display automatically with Windows in DS916 Settings first; sign-in handover needs it.'
}

# Reuse the app's Run-key and StartupApproved handling, without starting a tray.
$taskSetup = Start-Process -FilePath $taskApp -ArgumentList '--register-autostart' -PassThru -Wait -WindowStyle Hidden
if ($taskSetup.ExitCode -ne 0) {
    throw 'Could not register sign-in startup. Build and install the updated app first.'
}
$taskAction = New-ScheduledTaskAction -Execute $taskApp -Argument '--boot' -WorkingDirectory (Split-Path -Parent $taskApp)
$taskTrigger = New-ScheduledTaskTrigger -AtStartup
$taskUser = New-ScheduledTaskPrincipal -UserId $taskIdentity.Name -LogonType S4U -RunLevel Limited
$taskSettings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew `
    -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName $taskName -TaskPath '\' -Action $taskAction -Trigger $taskTrigger `
    -Principal $taskUser -Settings $taskSettings -Description 'Display the saved DS916 theme before sign-in; yield to the tray app after sign-in.' -Force | Out-Null
Write-Host 'DS916 boot display enabled. The tray app takes over after sign-in.'
Write-Host ('Boot log: ' + (Join-Path (Split-Path -Parent $taskApp) 'ds916_boot_log.txt'))
Write-Host 'No Windows password is stored. Live sensors need LibreHardwareMonitor to be running.'

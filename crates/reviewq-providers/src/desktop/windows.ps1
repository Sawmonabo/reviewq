$ErrorActionPreference = 'Stop'
$request = [Text.Encoding]::UTF8.GetString(
    [Convert]::FromBase64String([Console]::In.ReadToEnd())
) | ConvertFrom-Json
$app = 'Personal.ReviewQ'
$path = 'Software\Classes\AppUserModelId\' + $app
$registry = [Microsoft.Win32.Registry]::CurrentUser
$folder = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) $app
$icon = Join-Path $folder 'notification.png'
$marker = Join-Path $folder 'owner'

function OwnsPath([string] $target) {
    $item = Get-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue
    return $null -eq $item -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0
}

function ReadIconDigest {
    if (Test-Path -LiteralPath $icon -PathType Leaf) {
        return (Get-FileHash -LiteralPath $icon -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

if ($request.operation -eq 'show') {
    $entry = $registry.OpenSubKey($path)
    if ($null -eq $entry -or $entry.GetValue('ReviewQOwner') -cne $request.owner) { exit 20 }
    $entry.Dispose()
    try {
        [void][Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]
        [void][Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime]
        $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
        $xml.LoadXml('<toast activationType="protocol"><visual><binding template="ToastGeneric"><text/><text/></binding></visual></toast>')
        $xml.DocumentElement.SetAttribute('launch', $request.url)
        $text = $xml.GetElementsByTagName('text')
        [void]$text.Item(0).AppendChild($xml.CreateTextNode($request.title))
        [void]$text.Item(1).AppendChild($xml.CreateTextNode($request.body))
        $toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
        $toast.Tag = $request.tag
        $toast.Group = 'ReviewQ'
        $notifier = [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app)
        if ($notifier.Setting -ne 'Enabled') { exit 20 }
    } catch { exit 20 }
    try { $notifier.Show($toast) } catch { exit 21 }
    exit 0
}

$mutex = [System.Threading.Mutex]::new($false, 'Local\Personal.ReviewQ.Setup')
try { $locked = $mutex.WaitOne([TimeSpan]::FromSeconds(8)) }
catch [System.Threading.AbandonedMutexException] { $locked = $true }
if (!$locked) { exit 22 }
try {
    if (!(OwnsPath $folder) -or !(OwnsPath $marker) -or !(OwnsPath $icon)) { exit 22 }
    $entry = $registry.OpenSubKey($path)
    if ($null -ne $entry) {
        try {
            if ($entry.GetValue('ReviewQOwner') -cne $request.owner -or
                $entry.SubKeyCount -ne 0 -or
                @($entry.GetValueNames() | Where-Object {
                    $_ -notin @('ReviewQOwner', 'DisplayName', 'IconUri', 'IconBackgroundColor')
                }).Count -ne 0) { exit 22 }
            if ($entry.GetValue('DisplayName', 'ReviewQ') -cne 'ReviewQ' -or
                $entry.GetValue('IconUri', $icon) -cne $icon -or
                $entry.GetValue('IconBackgroundColor', 'FF111215') -cne 'FF111215') { exit 22 }
        } finally { $entry.Dispose() }
    }
    if (Test-Path -LiteralPath $folder) {
        if (!(Test-Path -LiteralPath $folder -PathType Container) -or
            !(Test-Path -LiteralPath $marker -PathType Leaf) -or
            [IO.File]::ReadAllText($marker) -cne $request.owner) { exit 22 }
        $digest = ReadIconDigest
        if ($null -ne $digest -and $digest -cne $request.digest -and
            $digest -cne $request.previous) { exit 22 }
    }
    if ($request.operation -eq 'check') { exit 0 }
    if ($request.operation -eq 'remove') {
        if ($null -ne $entry) { $registry.DeleteSubKey($path, $false) }
        if (Test-Path -LiteralPath $icon -PathType Leaf) { [IO.File]::Delete($icon) }
        if (Test-Path -LiteralPath $marker -PathType Leaf) { [IO.File]::Delete($marker) }
        if ((Test-Path -LiteralPath $folder -PathType Container) -and
            @(Get-ChildItem -LiteralPath $folder -Force).Count -eq 0) { [IO.Directory]::Delete($folder) }
        exit 0
    }
    if ($request.operation -ne 'install') { exit 22 }
    $directory = [IO.Directory]::CreateDirectory($folder)
    if (!(Test-Path -LiteralPath $marker)) { [IO.File]::WriteAllText($marker, $request.owner) }
    if ((ReadIconDigest) -cne $request.digest) {
        $stage = Join-Path $folder ('icon-' + $request.owner + '.pending')
        if (!(OwnsPath $stage)) { exit 22 }
        [IO.File]::WriteAllBytes($stage, [Convert]::FromBase64String($request.icon))
        if ((Get-FileHash -LiteralPath $stage -Algorithm SHA256).Hash.ToLowerInvariant() -cne $request.digest) { exit 22 }
        if (Test-Path -LiteralPath $icon) { [IO.File]::Replace($stage, $icon, $null) }
        else { [IO.File]::Move($stage, $icon) }
    }
    $registration = $registry.CreateSubKey($path)
    try {
        $registration.SetValue('ReviewQOwner', $request.owner)
        $registration.SetValue('DisplayName', 'ReviewQ')
        $registration.SetValue('IconUri', $icon)
        $registration.SetValue('IconBackgroundColor', 'FF111215')
    } finally { $registration.Dispose() }
    exit 0
} finally {
    $mutex.ReleaseMutex()
    $mutex.Dispose()
}

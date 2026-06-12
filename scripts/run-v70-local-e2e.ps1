param(
    [string]$Toolchain = "",
    [string]$TargetRoot = "",
    [int]$NodePort = 0,
    [int]$HealthPort = 0,
    [int]$GamePort = 0,
    [string]$Payload = "xaccel-v70-local-e2e",
    [int]$TimeoutSec = 30,
    [switch]$KeepTemp
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$isWindowsHost = ($env:OS -eq "Windows_NT") -or [bool](Get-Variable -Name IsWindows -ValueOnly -ErrorAction SilentlyContinue)
if ([string]::IsNullOrWhiteSpace($Toolchain)) {
    if ($isWindowsHost) {
        $Toolchain = "stable-x86_64-pc-windows-gnu"
    } else {
        $Toolchain = "stable"
    }
}
if ([string]::IsNullOrWhiteSpace($TargetRoot)) {
    if ($isWindowsHost) {
        $TargetRoot = "C:\xaccel-target\v70-local-e2e"
    } else {
        $TargetRoot = Join-Path ([System.IO.Path]::GetTempPath()) "xaccel-target-v70-local-e2e"
    }
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workDir = Join-Path ([System.IO.Path]::GetTempPath()) ("xaccel-v70-local-e2e-" + (Get-Date -Format "yyyyMMdd-HHmmss"))
$logDir = Join-Path $workDir "logs"
$nodeProcess = $null
$echoJob = $null
$succeeded = $false

function ConvertTo-PosixPath([string]$Path) {
    return $Path.Replace("\", "/")
}

function ConvertTo-Utf8Bytes([string]$Text) {
    return [System.Text.Encoding]::UTF8.GetBytes($Text)
}

function Write-Utf8NoBomFile([string]$Path, [string]$Content) {
    $encoding = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function ConvertTo-Base64UrlNoPad([byte[]]$Bytes) {
    return [Convert]::ToBase64String($Bytes).TrimEnd("=").Replace("+", "-").Replace("/", "_")
}

function Get-Sha256Base64Url([string]$Text) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ConvertTo-Base64UrlNoPad ($sha.ComputeHash((ConvertTo-Utf8Bytes $Text)))
    } finally {
        $sha.Dispose()
    }
}

function Get-HmacSha256Base64Url([string]$Secret, [string]$Text) {
    $hmac = [System.Security.Cryptography.HMACSHA256]::new((ConvertTo-Utf8Bytes $Secret))
    try {
        return ConvertTo-Base64UrlNoPad ($hmac.ComputeHash((ConvertTo-Utf8Bytes $Text)))
    } finally {
        $hmac.Dispose()
    }
}

function New-XAccelToken([object]$Claims, [string]$Secret) {
    $payloadJson = $Claims | ConvertTo-Json -Compress -Depth 32
    $payload = ConvertTo-Base64UrlNoPad (ConvertTo-Utf8Bytes $payloadJson)
    $signingInput = "xat.v1.$payload"
    $signature = Get-HmacSha256Base64Url $Secret $signingInput
    return "$signingInput.$signature"
}

function Test-TcpPortAvailable([int]$Port) {
    $listener = $null
    try {
        $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
        $listener.Start()
        return $true
    } catch {
        return $false
    } finally {
        if ($null -ne $listener) {
            $listener.Stop()
        }
    }
}

function Get-FreeLoopbackPort {
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        $udp = [System.Net.Sockets.UdpClient]::new(0)
        try {
            $port = ([System.Net.IPEndPoint]$udp.Client.LocalEndPoint).Port
        } finally {
            $udp.Dispose()
        }
        if (Test-TcpPortAvailable $port) {
            return $port
        }
    }
    throw "failed to find a free loopback port"
}

function Invoke-CargoBuild([string]$CrateDir, [string]$TargetDir) {
    New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null
    $oldTargetDir = $env:CARGO_TARGET_DIR
    $env:CARGO_TARGET_DIR = $TargetDir
    try {
        Push-Location $CrateDir
        $args = @()
        if (-not [string]::IsNullOrWhiteSpace($Toolchain)) {
            $args += "+$Toolchain"
        }
        $args += @("build", "--locked")
        & cargo @args
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed in $CrateDir"
        }
    } finally {
        Pop-Location
        $env:CARGO_TARGET_DIR = $oldTargetDir
    }
}

function Wait-ForFile([string]$Path, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $Path) {
            return
        }
        Start-Sleep -Milliseconds 100
    }
    throw "timed out waiting for $Path"
}

function Wait-ForHealth([string]$Uri, [System.Diagnostics.Process]$Process, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if ($Process.HasExited) {
            throw "node process exited before health check was ready"
        }
        try {
            $response = Invoke-WebRequest -UseBasicParsing -Uri $Uri -TimeoutSec 2
            if ($response.StatusCode -ge 200 -and $response.StatusCode -lt 300) {
                return
            }
        } catch {
            Start-Sleep -Milliseconds 250
        }
    }
    throw "timed out waiting for node health $Uri"
}

function Get-BinaryPath([string]$TargetDir, [string]$Name) {
    $binaryName = $Name
    if ($isWindowsHost) {
        $binaryName = "$Name.exe"
    }
    return Join-Path (Join-Path $TargetDir "debug") $binaryName
}

try {
    Get-Command cargo | Out-Null

    New-Item -ItemType Directory -Force -Path $workDir, $logDir | Out-Null
    if ($NodePort -eq 0) {
        $NodePort = Get-FreeLoopbackPort
    }
    if ($HealthPort -eq 0) {
        do {
            $HealthPort = Get-FreeLoopbackPort
        } while ($HealthPort -eq $NodePort)
    }
    if ($GamePort -eq 0) {
        do {
            $GamePort = Get-FreeLoopbackPort
        } while ($GamePort -eq $NodePort -or $GamePort -eq $HealthPort)
    }

    $nodeTargetDir = Join-Path $TargetRoot "node-core"
    $clientTargetDir = Join-Path $TargetRoot "client-probe"
    Write-Host "Building node-core with $Toolchain..."
    Invoke-CargoBuild (Join-Path $repoRoot "node-core") $nodeTargetDir
    Write-Host "Building client-probe with $Toolchain..."
    Invoke-CargoBuild (Join-Path $repoRoot "client-probe") $clientTargetDir

    $nodeExe = Get-BinaryPath $nodeTargetDir "xaccel-node"
    $clientExe = Get-BinaryPath $clientTargetDir "xaccel-client-probe"
    if (-not (Test-Path $nodeExe)) {
        throw "node binary not found: $nodeExe"
    }
    if (-not (Test-Path $clientExe)) {
        throw "client-probe binary not found: $clientExe"
    }

    $nodeId = 7001
    $userId = 1001
    $deviceId = "pc-v70-local-e2e"
    $gameId = 8888
    $regionId = 1
    $intentId = "intent-v70-local-e2e"
    $nodeSecret = "local-v70-secret-$([Guid]::NewGuid().ToString("N"))"
    $issuedAt = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $expiresAt = $issuedAt + 300

    $identityPath = Join-Path $workDir "identity.json"
    $configPath = Join-Path $workDir "node-config.toml"
    $ticketPath = Join-Path $workDir "accel-ticket.json"
    $echoReadyPath = Join-Path $workDir "udp-echo.ready"
    $echoLogPath = Join-Path $logDir "udp-echo.log"
    $nodeOutPath = Join-Path $logDir "node.out.log"
    $nodeErrPath = Join-Path $logDir "node.err.log"

    $identity = [ordered]@{
        node_id = $nodeId
        panel_url = "http://127.0.0.1:18080"
        node_secret = $nodeSecret
        created_by_installer = $true
    }
    Write-Utf8NoBomFile $identityPath ($identity | ConvertTo-Json -Compress -Depth 8)

    $configToml = @"
[identity]
node_id = $nodeId
panel_url = "http://127.0.0.1:18080"
identity_file = "$(ConvertTo-PosixPath $identityPath)"

[runtime]
data_dir = "$(ConvertTo-PosixPath (Join-Path $workDir "data"))"
log_dir = "$(ConvertTo-PosixPath $logDir)"
health_addr = "127.0.0.1:$HealthPort"
channel = "local"

[network]
server_ip = "127.0.0.1"
listen_ip = "127.0.0.1"
server_port = $NodePort
relay_server_ip = ""
relay_server_port = 0
is_support_ipv6 = false
disable_quic = false
area = "LOCAL"
bandwidth_quality = "normal"
tag = "v70-local"

[control]
enabled = false
config_revision = 1
request_timeout_sec = 5
config_poll_interval_sec = 30

[report]
interval_sec = 30
traffic_batch_sec = 60
metrics_interval_sec = 15

[limits]
max_sessions = 1024
max_sessions_per_user = 16
max_udp_mappings = 4096
default_user_speed_mbps = 0
"@
    Write-Utf8NoBomFile $configPath $configToml

    $routePolicy = [ordered]@{
        policy_id = "rp-v70-local-e2e"
        policy_version = 1
        mode = "dynamic_targets"
        default_protocol = "udp"
        targets = @(
            [ordered]@{
                target_id = "local-udp-echo"
                host_type = "observed_ip"
                resolved_ips = @()
                observed_ips = @("127.0.0.1")
                cidrs = @()
                ports = @(
                    [ordered]@{
                        protocol = "udp"
                        from = $GamePort
                        to = $GamePort
                    }
                )
            }
        )
    }
    $routePolicyJson = $routePolicy | ConvertTo-Json -Compress -Depth 32
    $routePolicyHash = Get-Sha256Base64Url $routePolicyJson
    $claims = [ordered]@{
        node_id = $nodeId
        user_id = $userId
        device_id = $deviceId
        game_id = $gameId
        region_id = $regionId
        intent_id = $intentId
        route_policy_hash = $routePolicyHash
        route_policy_id = $routePolicy["policy_id"]
        expires_at = $expiresAt
        issued_at = $issuedAt
        nonce = "nonce-$([Guid]::NewGuid().ToString("N"))"
    }
    $token = New-XAccelToken $claims $nodeSecret

    $ticket = [ordered]@{
        ticket_id = $intentId
        ttl_sec = 300
        issue_mode = "per_session"
        client = [ordered]@{
            user_id = $userId
            device_id = $deviceId
            game_id = $gameId
            region_id = $regionId
        }
        node = [ordered]@{
            node_id = $nodeId
            host = "127.0.0.1"
            port = $NodePort
            area = "LOCAL"
            tag = "v70-local"
            transports = @("udp")
            bandwidth_quality = "normal"
        }
        route = [ordered]@{
            target_addr = "127.0.0.1:$GamePort"
            protocol = "udp"
            region_id = $regionId
            region_name = "Local E2E"
        }
        route_policy = $routePolicy
        credential = [ordered]@{
            token = $token
            expires_at = $expiresAt
            intent_id = $intentId
        }
    }
    Write-Utf8NoBomFile $ticketPath ($ticket | ConvertTo-Json -Depth 32)

    $echoJob = Start-Job -Name "xaccel-v70-udp-echo" -ArgumentList $GamePort, $echoReadyPath, $echoLogPath -ScriptBlock {
        param([int]$Port, [string]$ReadyPath, [string]$LogPath)
        $udp = [System.Net.Sockets.UdpClient]::new([System.Net.IPEndPoint]::new([System.Net.IPAddress]::Parse("127.0.0.1"), $Port))
        try {
            Set-Content -Encoding UTF8 -Path $ReadyPath -Value "ready"
            while ($true) {
                $remote = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)
                $bytes = $udp.Receive([ref]$remote)
                $text = [System.Text.Encoding]::UTF8.GetString($bytes)
                Add-Content -Encoding UTF8 -Path $LogPath -Value ("{0} {1} {2}" -f ([DateTimeOffset]::UtcNow.ToUnixTimeSeconds()), $remote, $text)
                $responseText = "echo:$text"
                $responseBytes = [System.Text.Encoding]::UTF8.GetBytes($responseText)
                [void]$udp.Send($responseBytes, $responseBytes.Length, $remote)
            }
        } finally {
            $udp.Dispose()
        }
    }
    Wait-ForFile $echoReadyPath $TimeoutSec

    Write-Host "Starting local node on 127.0.0.1:$NodePort..."
    $nodeStart = @{
        FilePath = $nodeExe
        ArgumentList = @("--config", "`"$configPath`"")
        RedirectStandardOutput = $nodeOutPath
        RedirectStandardError = $nodeErrPath
        PassThru = $true
    }
    if ($isWindowsHost) {
        $nodeStart["WindowStyle"] = "Hidden"
    }
    $nodeProcess = Start-Process @nodeStart
    Wait-ForHealth "http://127.0.0.1:$HealthPort/health" $nodeProcess $TimeoutSec

    Write-Host "Running client-probe with dynamic route_policy ticket..."
    $clientOutput = & $clientExe `
        --accel-ticket-file $ticketPath `
        --payload $Payload `
        --timeout-sec 5 `
        --response-timeout-ms 1000 `
        --compact 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "client-probe failed: $clientOutput"
    }
    $summaryValue = $clientOutput | Where-Object { $_.ToString().Trim().StartsWith("{") } | Select-Object -Last 1
    if ($null -eq $summaryValue) {
        throw "client-probe did not print a JSON summary: $clientOutput"
    }
    $summaryLine = $summaryValue.ToString()
    $summary = $summaryLine | ConvertFrom-Json
    if ($summary.status -ne "ok") {
        throw "client-probe status is not ok: $summaryLine"
    }
    if ($null -eq $summary.session_data) {
        throw "client-probe did not run session_data: $summaryLine"
    }
    if ($summary.session_data.status -ne "forwarded") {
        throw "session_data was not forwarded: $summaryLine"
    }
    $expectedPayload = "echo:$Payload"
    if ($summary.session_data.response_payload_text -ne $expectedPayload) {
        throw "unexpected response payload. expected '$expectedPayload', got '$($summary.session_data.response_payload_text)'"
    }
    if ($summary.session_data.target_id -ne "local-udp-echo") {
        throw "unexpected target_id: $($summary.session_data.target_id)"
    }
    if ($summary.session_data.matched_policy -ne $routePolicy["policy_id"]) {
        throw "unexpected matched_policy: $($summary.session_data.matched_policy)"
    }

    Write-Host "v0.70 local E2E ok"
    Write-Host "node: 127.0.0.1:$NodePort"
    Write-Host "health: http://127.0.0.1:$HealthPort/health"
    Write-Host "udp target: 127.0.0.1:$GamePort"
    Write-Host "ticket: $ticketPath"
    Write-Host "logs: $logDir"
    Write-Output $summaryLine
    $succeeded = $true
} finally {
    if ($null -ne $nodeProcess -and -not $nodeProcess.HasExited) {
        Stop-Process -Id $nodeProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $echoJob) {
        Stop-Job $echoJob -ErrorAction SilentlyContinue
        Remove-Job $echoJob -Force -ErrorAction SilentlyContinue
    }
    if (-not $KeepTemp -and $succeeded) {
        # Keep temp files on failures for diagnostics; successful runs can be reproduced anytime.
        Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

param(
    [string]$Toolchain = "",
    [string]$TargetRoot = "",
    [int]$NodePort = 0,
    [int]$HealthPort = 0,
    [int]$QuicPort = 0,
    [int]$UdpTargetPort = 0,
    [int]$TcpTargetPort = 0,
    [string]$Payload = "xaccel-node-tunnel-e2e",
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
        $TargetRoot = "C:\xaccel-target\node-tunnel-e2e"
    } else {
        $TargetRoot = Join-Path ([System.IO.Path]::GetTempPath()) "xaccel-target-node-tunnel-e2e"
    }
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workDir = Join-Path ([System.IO.Path]::GetTempPath()) ("xaccel-node-tunnel-e2e-" + (Get-Date -Format "yyyyMMdd-HHmmss"))
$logDir = Join-Path $workDir "logs"
$nodeProcess = $null
$udpEchoJob = $null
$tcpEchoJob = $null
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
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
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

function Invoke-UdpJson([int]$Port, [object]$Body) {
    $json = ($Body | ConvertTo-Json -Compress -Depth 32) + "`n"
    $bytes = ConvertTo-Utf8Bytes $json
    $udp = [System.Net.Sockets.UdpClient]::new()
    try {
        $udp.Client.ReceiveTimeout = 3000
        $endpoint = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Loopback, $Port)
        [void]$udp.Send($bytes, $bytes.Length, $endpoint)
        $remote = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)
        $responseBytes = $udp.Receive([ref]$remote)
        return ([System.Text.Encoding]::UTF8.GetString($responseBytes) | ConvertFrom-Json)
    } finally {
        $udp.Dispose()
    }
}

function New-NodeTcpConnection([int]$Port) {
    $client = [System.Net.Sockets.TcpClient]::new()
    $client.Connect("127.0.0.1", $Port)
    $stream = $client.GetStream()
    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
    $writer = [System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false), 4096, $true)
    $writer.NewLine = "`n"
    return [pscustomobject]@{
        Client = $client
        Reader = $reader
        Writer = $writer
    }
}

function Invoke-TcpJsonLine([object]$Connection, [object]$Body) {
    $json = $Body | ConvertTo-Json -Compress -Depth 32
    $Connection.Writer.WriteLine($json)
    $Connection.Writer.Flush()
    $line = $Connection.Reader.ReadLine()
    if ([string]::IsNullOrWhiteSpace($line)) {
        throw "TCP JSON channel returned an empty response"
    }
    return $line | ConvertFrom-Json
}

function Add-U16BE([System.Collections.Generic.List[byte]]$Bytes, [int]$Value) {
    [void]$Bytes.Add([byte](($Value -shr 8) -band 0xff))
    [void]$Bytes.Add([byte]($Value -band 0xff))
}

function Add-U32BE([System.Collections.Generic.List[byte]]$Bytes, [int]$Value) {
    [void]$Bytes.Add([byte](($Value -shr 24) -band 0xff))
    [void]$Bytes.Add([byte](($Value -shr 16) -band 0xff))
    [void]$Bytes.Add([byte](($Value -shr 8) -band 0xff))
    [void]$Bytes.Add([byte]($Value -band 0xff))
}

function New-RawUdpTunnelFrame(
    [string]$SessionId,
    [string]$TargetId,
    [string]$TargetHost,
    [int]$Port,
    [byte[]]$PayloadBytes
) {
    $sessionBytes = ConvertTo-Utf8Bytes $SessionId
    $targetBytes = ConvertTo-Utf8Bytes $TargetId
    $hostBytes = ConvertTo-Utf8Bytes $TargetHost
    $bytes = [System.Collections.Generic.List[byte]]::new()
    foreach ($byte in (ConvertTo-Utf8Bytes "XAU1")) { [void]$bytes.Add($byte) }
    [void]$bytes.Add(1)
    [void]$bytes.Add(1)
    [void]$bytes.Add(0)
    [void]$bytes.Add(0)
    Add-U16BE $bytes $sessionBytes.Length
    Add-U16BE $bytes $targetBytes.Length
    Add-U16BE $bytes $hostBytes.Length
    Add-U16BE $bytes $Port
    Add-U32BE $bytes $PayloadBytes.Length
    foreach ($byte in $sessionBytes) { [void]$bytes.Add($byte) }
    foreach ($byte in $targetBytes) { [void]$bytes.Add($byte) }
    foreach ($byte in $hostBytes) { [void]$bytes.Add($byte) }
    foreach ($byte in $PayloadBytes) { [void]$bytes.Add($byte) }
    return $bytes.ToArray()
}

function Read-U16BE([byte[]]$Bytes, [int]$Offset) {
    return ([int]$Bytes[$Offset] -shl 8) -bor [int]$Bytes[$Offset + 1]
}

function Read-U32BE([byte[]]$Bytes, [int]$Offset) {
    return ([int]$Bytes[$Offset] -shl 24) -bor ([int]$Bytes[$Offset + 1] -shl 16) -bor ([int]$Bytes[$Offset + 2] -shl 8) -bor [int]$Bytes[$Offset + 3]
}

function Invoke-RawUdpTunnel([int]$Port, [byte[]]$Frame) {
    $udp = [System.Net.Sockets.UdpClient]::new()
    try {
        $udp.Client.ReceiveTimeout = 3000
        $endpoint = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Loopback, $Port)
        [void]$udp.Send($Frame, $Frame.Length, $endpoint)
        $remote = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)
        $response = $udp.Receive([ref]$remote)
        $magic = [System.Text.Encoding]::ASCII.GetString($response, 0, 4)
        if ($magic -ne "XAU1") {
            throw "raw UDP response magic mismatch"
        }
        if ($response[4] -ne 1 -or $response[5] -ne 2) {
            throw "raw UDP response version or kind mismatch"
        }
        $sessionLen = Read-U16BE $response 8
        $statusLen = Read-U16BE $response 10
        $payloadLen = Read-U32BE $response 16
        $offset = 20
        $session = [System.Text.Encoding]::UTF8.GetString($response, $offset, $sessionLen)
        $offset += $sessionLen
        $status = [System.Text.Encoding]::UTF8.GetString($response, $offset, $statusLen)
        $offset += $statusLen
        $payloadBytes = $response[$offset..($offset + $payloadLen - 1)]
        if ($payloadLen -eq 0) {
            $payloadBytes = [byte[]]@()
        }
        return [pscustomobject]@{
            StatusCode = $response[6]
            SessionId = $session
            Status = $status
            PayloadBytes = [byte[]]$payloadBytes
            PayloadText = [System.Text.Encoding]::UTF8.GetString([byte[]]$payloadBytes)
        }
    } finally {
        $udp.Dispose()
    }
}

function Test-QuicPortBound([int]$Port) {
    if (Get-Command Get-NetUDPEndpoint -ErrorAction SilentlyContinue) {
        return [bool](Get-NetUDPEndpoint -LocalAddress 127.0.0.1 -LocalPort $Port -ErrorAction SilentlyContinue)
    }
    return $true
}

try {
    Get-Command cargo | Out-Null

    New-Item -ItemType Directory -Force -Path $workDir, $logDir | Out-Null
    $usedPorts = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($name in @("NodePort", "HealthPort", "QuicPort", "UdpTargetPort", "TcpTargetPort")) {
        $value = Get-Variable -Name $name -ValueOnly
        if ($value -ne 0) {
            [void]$usedPorts.Add($value)
        }
    }
    foreach ($name in @("NodePort", "HealthPort", "QuicPort", "UdpTargetPort", "TcpTargetPort")) {
        if ((Get-Variable -Name $name -ValueOnly) -eq 0) {
            do {
                $port = Get-FreeLoopbackPort
            } while ($usedPorts.Contains($port))
            Set-Variable -Name $name -Value $port
            [void]$usedPorts.Add($port)
        }
    }

    $nodeTargetDir = Join-Path $TargetRoot "node-core"
    Write-Host "Building node-core with $Toolchain..."
    Invoke-CargoBuild (Join-Path $repoRoot "node-core") $nodeTargetDir
    $nodeExe = Get-BinaryPath $nodeTargetDir "xaccel-node"
    if (-not (Test-Path $nodeExe)) {
        throw "node binary not found: $nodeExe"
    }

    $nodeId = 7101
    $userId = 1001
    $deviceId = "pc-node-tunnel-e2e"
    $gameId = 8888
    $regionId = 1
    $intentId = "intent-node-tunnel-e2e"
    $nodeSecret = "local-node-tunnel-secret-$([Guid]::NewGuid().ToString("N"))"
    $issuedAt = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $expiresAt = $issuedAt + 300

    $identityPath = Join-Path $workDir "identity.json"
    $configPath = Join-Path $workDir "node-config.toml"
    $udpReadyPath = Join-Path $workDir "udp-echo.ready"
    $tcpReadyPath = Join-Path $workDir "tcp-echo.ready"
    $udpLogPath = Join-Path $logDir "udp-echo.log"
    $tcpLogPath = Join-Path $logDir "tcp-echo.log"
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
relay_server_ip = "127.0.0.1"
relay_server_port = $QuicPort
is_support_ipv6 = false
disable_quic = false
area = "LOCAL"
bandwidth_quality = "normal"
tag = "node-tunnel-e2e"

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
        policy_id = "rp-node-tunnel-e2e"
        policy_version = 1
        mode = "dynamic_targets"
        default_protocol = "udp"
        targets = @(
            [ordered]@{
                target_id = "udp-echo"
                host_type = "observed_ip"
                resolved_ips = @()
                observed_ips = @("127.0.0.1")
                cidrs = @()
                ports = @([ordered]@{ protocol = "udp"; from = $UdpTargetPort; to = $UdpTargetPort })
            },
            [ordered]@{
                target_id = "tcp-echo"
                host_type = "observed_ip"
                resolved_ips = @()
                observed_ips = @("127.0.0.1")
                cidrs = @()
                ports = @([ordered]@{ protocol = "tcp"; from = $TcpTargetPort; to = $TcpTargetPort })
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

    $udpEchoJob = Start-Job -Name "xaccel-node-tunnel-udp-echo" -ArgumentList $UdpTargetPort, $udpReadyPath, $udpLogPath -ScriptBlock {
        param([int]$Port, [string]$ReadyPath, [string]$LogPath)
        $udp = [System.Net.Sockets.UdpClient]::new([System.Net.IPEndPoint]::new([System.Net.IPAddress]::Parse("127.0.0.1"), $Port))
        try {
            Set-Content -Encoding UTF8 -Path $ReadyPath -Value "ready"
            while ($true) {
                $remote = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)
                $bytes = $udp.Receive([ref]$remote)
                $text = [System.Text.Encoding]::UTF8.GetString($bytes)
                Add-Content -Encoding UTF8 -Path $LogPath -Value ("{0} {1} {2}" -f ([DateTimeOffset]::UtcNow.ToUnixTimeSeconds()), $remote, $text)
                $responseBytes = [System.Text.Encoding]::UTF8.GetBytes("udp:$text")
                [void]$udp.Send($responseBytes, $responseBytes.Length, $remote)
            }
        } finally {
            $udp.Dispose()
        }
    }
    $tcpEchoJob = Start-Job -Name "xaccel-node-tunnel-tcp-echo" -ArgumentList $TcpTargetPort, $tcpReadyPath, $tcpLogPath -ScriptBlock {
        param([int]$Port, [string]$ReadyPath, [string]$LogPath)
        $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Parse("127.0.0.1"), $Port)
        $listener.Start()
        try {
            Set-Content -Encoding UTF8 -Path $ReadyPath -Value "ready"
            while ($true) {
                $client = $listener.AcceptTcpClient()
                try {
                    $stream = $client.GetStream()
                    $client.ReceiveTimeout = 1000
                    $buffer = New-Object byte[] 4096
                    $bytes = New-Object System.Collections.Generic.List[byte]
                    $read = $stream.Read($buffer, 0, $buffer.Length)
                    while ($read -gt 0) {
                        for ($i = 0; $i -lt $read; $i++) {
                            [void]$bytes.Add($buffer[$i])
                        }
                        if (-not $stream.DataAvailable) {
                            break
                        }
                        $read = $stream.Read($buffer, 0, $buffer.Length)
                    }
                    $text = [System.Text.Encoding]::UTF8.GetString($bytes.ToArray())
                    Add-Content -Encoding UTF8 -Path $LogPath -Value ("{0} {1}" -f ([DateTimeOffset]::UtcNow.ToUnixTimeSeconds()), $text)
                    $responseBytes = [System.Text.Encoding]::UTF8.GetBytes("tcp:$text")
                    $stream.Write($responseBytes, 0, $responseBytes.Length)
                    $stream.Flush()
                } finally {
                    $client.Close()
                }
            }
        } finally {
            $listener.Stop()
        }
    }
    Wait-ForFile $udpReadyPath $TimeoutSec
    Wait-ForFile $tcpReadyPath $TimeoutSec

    Write-Host "Starting local node on 127.0.0.1:$NodePort with QUIC 127.0.0.1:$QuicPort..."
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
    if (-not (Test-QuicPortBound $QuicPort)) {
        throw "QUIC UDP endpoint was not bound on 127.0.0.1:$QuicPort"
    }

    Write-Host "Running UDP probe and raw XAU1 tunnel..."
    $probeRequest = [ordered]@{
        type = "probe"
        protocol = "xaccel/1"
        client_nonce = "probe-udp"
        user_id = $userId
        device_id = $deviceId
        game_id = $gameId
        region_id = $regionId
        transport = "udp"
        token = $token
        route_policy = $routePolicy
    }
    $udpProbe = Invoke-UdpJson $NodePort $probeRequest
    if ($udpProbe.type -ne "probe.ok" -or -not $udpProbe.session.credential_valid) {
        throw "UDP probe failed: $($udpProbe | ConvertTo-Json -Compress -Depth 16)"
    }

    $rawPayload = ConvertTo-Utf8Bytes $Payload
    $rawFrame = New-RawUdpTunnelFrame $udpProbe.session.session_id "udp-echo" "127.0.0.1" $UdpTargetPort $rawPayload
    $rawResponse = Invoke-RawUdpTunnel $NodePort $rawFrame
    if ($rawResponse.Status -ne "forwarded" -or $rawResponse.PayloadText -ne "udp:$Payload") {
        throw "raw UDP tunnel failed: $($rawResponse | ConvertTo-Json -Compress -Depth 8)"
    }

    Write-Host "Running TCP long-lived JSON channel and TCP target relay..."
    $tcpConnection = New-NodeTcpConnection $NodePort
    try {
        $tcpProbeRequest = [ordered]@{
            type = "probe"
            protocol = "xaccel/1"
            client_nonce = "probe-tcp"
            user_id = $userId
            device_id = $deviceId
            game_id = $gameId
            region_id = $regionId
            transport = "tcp"
            token = $token
            route_policy = $routePolicy
        }
        $tcpProbe = Invoke-TcpJsonLine $tcpConnection $tcpProbeRequest
        if ($tcpProbe.type -ne "probe.ok" -or -not $tcpProbe.session.credential_valid) {
            throw "TCP probe failed: $($tcpProbe | ConvertTo-Json -Compress -Depth 16)"
        }

        for ($i = 1; $i -le 2; $i++) {
            $tcpPayload = "$Payload-tcp-$i"
            $sessionData = [ordered]@{
                type = "session.data"
                protocol = "xaccel/1"
                session_id = $tcpProbe.session.session_id
                client_nonce = "tcp-data-$i"
                target = [ordered]@{
                    target_id = "tcp-echo"
                    protocol = "tcp"
                    host = "127.0.0.1"
                    port = $TcpTargetPort
                }
                payload = [Convert]::ToBase64String((ConvertTo-Utf8Bytes $tcpPayload))
                response_timeout_ms = 1000
            }
            $tcpResponse = Invoke-TcpJsonLine $tcpConnection $sessionData
            $responseText = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($tcpResponse.payload))
            if ($tcpResponse.status -ne "forwarded" -or $tcpResponse.relay.mode -ne "tcp_target" -or $responseText -ne "tcp:$tcpPayload") {
                throw "TCP relay failed: $($tcpResponse | ConvertTo-Json -Compress -Depth 16)"
            }
        }
    } finally {
        $tcpConnection.Writer.Dispose()
        $tcpConnection.Reader.Dispose()
        $tcpConnection.Client.Close()
    }

    Write-Host "node tunnel E2E ok"
    Write-Host "node: 127.0.0.1:$NodePort"
    Write-Host "health: http://127.0.0.1:$HealthPort/health"
    Write-Host "quic: 127.0.0.1:$QuicPort"
    Write-Host "udp target: 127.0.0.1:$UdpTargetPort"
    Write-Host "tcp target: 127.0.0.1:$TcpTargetPort"
    Write-Host "logs: $logDir"
    Write-Output (@{
        status = "ok"
        node_version = "0.40.0"
        raw_udp = @{
            status = $rawResponse.Status
            payload = $rawResponse.PayloadText
        }
        tcp_long_lived = @{
            frames = 2
            relay = "tcp_target"
        }
        quic = @{
            listen = "127.0.0.1:$QuicPort"
            bound = $true
        }
    } | ConvertTo-Json -Compress -Depth 8)
    $succeeded = $true
} finally {
    if ($null -ne $nodeProcess -and -not $nodeProcess.HasExited) {
        Stop-Process -Id $nodeProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $udpEchoJob) {
        Stop-Job $udpEchoJob -ErrorAction SilentlyContinue
        Remove-Job $udpEchoJob -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $tcpEchoJob) {
        Stop-Job $tcpEchoJob -ErrorAction SilentlyContinue
        Remove-Job $tcpEchoJob -Force -ErrorAction SilentlyContinue
    }
    if (-not $KeepTemp -and $succeeded) {
        Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

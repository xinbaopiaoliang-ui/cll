$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$broadRoot = Split-Path -Parent $root
$xboardResearch = Join-Path $broadRoot "Xboard\game-accelerator-research"

if (-not (Test-Path -LiteralPath $root)) {
    throw "research root not found: $root"
}

if (Test-Path -LiteralPath $xboardResearch) {
    throw "unexpected research directory inside Xboard: $xboardResearch"
}

$required = @(
    "README.md",
    "docs\domain-model.md",
    "docs\linux-node-kernel-design.md",
    "docs\node-config-and-api.md",
    "docs\one-click-install-design.md",
    "docs\admin-node-install-flow.md",
    "docs\node-ops-upgrade.md",
    "docs\deploy-linux.md",
    "docs\control-api-mysql.md",
    "api\openapi-node.yaml",
    "db\schema.sql",
    "install\install.sh",
    "install\control-api-install.sh",
    "install\uninstall.sh",
    "install\control-api-uninstall.sh",
    "install\release-manifest.example.json",
    "install\config.example.toml",
    "install\systemd\xaccel-node.service",
    "install\systemd\xaccel-control-api.service",
    "node-core\Cargo.toml",
    "node-core\src\main.rs",
    "node-core\src\listener.rs",
    "backend-mock\Cargo.toml",
    "backend-mock\Cargo.lock",
    "backend-mock\src\main.rs",
    "backend-mock\README.md",
    "control-api\Cargo.toml",
    "control-api\Cargo.lock",
    "control-api\src\main.rs",
    "control-api\README.md",
    "client-probe\Cargo.toml",
    "client-probe\Cargo.lock",
    "client-probe\src\main.rs",
    "client-probe\README.md",
    "db\control-api.seed.example.sql",
    "scripts\package-release.sh",
    "scripts\package-control-api-release.sh",
    "scripts\package-client-probe-release.sh",
    ".github\workflows\release.yml",
    "docs\client-probe.md",
    "docs\local-validation.md"
)

foreach ($path in $required) {
    $full = Join-Path $root $path
    if (-not (Test-Path -LiteralPath $full)) {
        throw "missing required file: $full"
    }
}

Get-ChildItem -Recurse -File -LiteralPath $root |
    Where-Object {
        $_.FullName -notmatch "\\(\.git|target|dist)\\"
    } |
    Sort-Object FullName |
    Select-Object FullName, Length

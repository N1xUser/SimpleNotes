$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    cargo fmt --check
    if ($LASTEXITCODE -ne 0) { throw 'Formatting check failed.' }
    cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
    cargo clippy --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Static analysis failed.' }
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }
    New-Item -ItemType Directory -Path (Join-Path $PSScriptRoot 'dist') -Force | Out-Null
    $notesTarget = Join-Path $PSScriptRoot 'dist\SimpleNotes.exe'
    $notesStaging = Join-Path $PSScriptRoot 'dist\SimpleNotes.next.exe'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\simplenotes.exe') -Destination $notesStaging -Force
    try {
        Move-Item -LiteralPath $notesStaging -Destination $notesTarget -Force
    } catch {
        if (-not (Test-Path -LiteralPath $notesTarget)) { throw }
        $notesPrevious = Join-Path $PSScriptRoot ('dist\SimpleNotes.previous-' + [DateTime]::UtcNow.Ticks + '.exe')
        # Windows permits renaming a running image. Keep it intact while its notes are open.
        Move-Item -LiteralPath $notesTarget -Destination $notesPrevious
        try {
            Move-Item -LiteralPath $notesStaging -Destination $notesTarget
        } catch {
            Move-Item -LiteralPath $notesPrevious -Destination $notesTarget
            throw
        }
        Write-Host 'The previous version is still running. Exit from its tray menu and open SimpleNotes.exe to use the update.'
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README.md') -Destination (Join-Path $PSScriptRoot 'dist\README.md') -Force
    Write-Host 'Ready: dist\SimpleNotes.exe'
} finally {
    Pop-Location
}

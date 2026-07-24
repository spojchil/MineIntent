$ErrorActionPreference = 'Stop'

$manager = Join-Path $PSScriptRoot 'mc_manager.py'
if (-not (Test-Path -LiteralPath $manager)) {
    throw "Missing manager: $manager"
}

& python $manager @args
exit $LASTEXITCODE

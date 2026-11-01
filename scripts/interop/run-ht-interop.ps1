$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$run = Join-Path $root '.interop\ht-run'
New-Item -ItemType Directory -Force $run | Out-Null
$oracle = Join-Path $root '.interop\j2k-oracle\bin\Release\net8.0\win-x64\j2k-oracle.exe'
$tool = Join-Path $root 'target\debug\examples\j2k_tool.exe'
if (!(Test-Path $tool)) { throw 'build j2k_tool first' }

function Invoke-Tool([string[]]$arguments) {
    & $tool @arguments
    if ($LASTEXITCODE -ne 0) { throw "j2k_tool failed: $arguments" }
}
function Invoke-Oracle([string[]]$arguments) {
    & $oracle @arguments
    if ($LASTEXITCODE -ne 0) { throw "oracle failed: $arguments" }
}
function Compare-Bytes([string]$a, [string]$b, [double]$meanLimit, [int]$maxLimit) {
    $x = [IO.File]::ReadAllBytes($a)
    $y = [IO.File]::ReadAllBytes($b)
    if ($x.Length -ne $y.Length) { return @{ ok = $false; detail = "length $($x.Length) vs $($y.Length)" } }
    $total = 0L; $max = 0
    for ($i = 0; $i -lt $x.Length; $i++) {
        $d = [Math]::Abs([int]$x[$i] - [int]$y[$i])
        $total += $d
        if ($d -gt $max) { $max = $d }
    }
    $mean = if ($x.Length -gt 0) { $total / $x.Length } else { 0 }
    @{ ok = ($max -le $maxLimit -and $mean -le $meanLimit); mean = [Math]::Round($mean, 4); max = $max }
}

$cases = @(
    @{ name = 'mono8'; rows = 48; cols = 64; bits = 8; samples = 1; photo = 'MONOCHROME2' },
    @{ name = 'rgb8'; rows = 48; cols = 64; bits = 8; samples = 3; photo = 'RGB' }
)
$uids = @(
    @{ uid = '1.2.840.10008.1.2.4.201'; tag = 'lossless'; reversible = $true; rpcl = $false; mean = 0.2; max = 1 },
    @{ uid = '1.2.840.10008.1.2.4.202'; tag = 'rpcl'; reversible = $true; rpcl = $true; mean = 0.2; max = 1 },
    @{ uid = '1.2.840.10008.1.2.4.203'; tag = 'lossy'; reversible = $false; rpcl = $false; mean = 3.0; max = 255 },
    @{ uid = '1.2.840.10008.1.2.4.202'; tag = 'rpcl-tlm'; reversible = $true; rpcl = $true; tlm = $true; mean = 0.2; max = 1 },
    @{ uid = '1.2.840.10008.1.2.4.203'; tag = 'lossy-q40'; reversible = $false; rpcl = $false; quality = 40; mean = 12.0; max = 255 }
)

foreach ($c in $cases) {
    $base = Join-Path $run $c.name
    $pixels = $c.rows * $c.cols * $c.samples
    $raw = New-Object byte[] $pixels
    for ($i = 0; $i -lt $pixels; $i++) { $raw[$i] = [byte](($i * 37 + 11) % 256) }
    [IO.File]::WriteAllBytes("$base.raw", $raw)
    $meta = [ordered]@{
        rows = $c.rows; cols = $c.cols; bitsAllocated = 8; bitsStored = 8;
        samplesPerPixel = $c.samples; photometric = $c.photo;
        pixelRepresentation = 0; planarConfiguration = 0; numberOfFrames = 1;
        transferSyntaxUid = ''; frameLengths = @($raw.Length)
    }
    foreach ($u in $uids) {
        $meta.transferSyntaxUid = $u.uid
        $metaPath = "$base-$($u.tag).meta.json"
        $meta | ConvertTo-Json -Compress | Set-Content $metaPath -NoNewline
        $args = @('encode', '--rows', $c.rows, '--cols', $c.cols, '--bits', 8,
                  '--samples', $c.samples, '--photometric', $c.photo, '--raw', "$base.raw",
                  '--ht', '--out', "$base-rust-$($u.tag).j2k")
        if ($u.reversible) { $args += '--reversible' }
        if ($u.rpcl) { $args += '--rpcl' }
        if ($u.tlm) { $args += '--tlm' }
        if ($u.quality) { $args += @('--quality', $u.quality) }
        Invoke-Tool $args
        Invoke-Oracle @('wrap', '--syntax', $u.uid, '--codestream', "$base-rust-$($u.tag).j2k", '--meta', $metaPath, '--out', "$base-rust-$($u.tag).dcm")
        Invoke-Oracle @('decode', '--in', "$base-rust-$($u.tag).dcm", '--out', "$base-fo-$($u.tag).raw", '--meta', "$base-fo-$($u.tag).meta.json")
        $r = Compare-Bytes "$base.raw" "$base-fo-$($u.tag).raw" $u.mean $u.max
        "Rust->fo-dicom $($c.name) $($u.tag) ok=$($r.ok) mean=$($r.mean) max=$($r.max)"

        # fo-dicom exposes no quality knob here, so quality cases check the
        # forward direction only.
        if ($u.quality) { continue }
        Invoke-Oracle @('ingest', '--syntax', $u.uid, '--raw', "$base.raw", '--meta', $metaPath, '--out', "$base-fo-$($u.tag).dcm")
        Invoke-Oracle @('extract', '--in', "$base-fo-$($u.tag).dcm", '--outdir', "$base-fo-$($u.tag)")
        Invoke-Oracle @('decode', '--in', "$base-fo-$($u.tag).dcm", '--out', "$base-fo-$($u.tag)-ref.raw", '--meta', "$base-fo-$($u.tag)-ref.meta.json")
        $args = @('decode', '--syntax', $u.uid, '--rows', $c.rows, '--cols', $c.cols, '--bits', 8,
                  '--samples', $c.samples, '--photometric', $c.photo, '--in', "$base-fo-$($u.tag)\frame-0.j2k",
                  '--out', "$base-rust-$($u.tag).raw")
        Invoke-Tool $args
        $reference = if ($u.reversible) { "$base.raw" } else { "$base-fo-$($u.tag)-ref.raw" }
        $r = Compare-Bytes $reference "$base-rust-$($u.tag).raw" $u.mean $u.max
        "fo-dicom->Rust $($c.name) $($u.tag) ok=$($r.ok) mean=$($r.mean) max=$($r.max)"
    }
}

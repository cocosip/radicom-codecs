# DICOM 编解码器性能测试与续跑记录

2026-11-26。Rust 优化前的基线为 `b94892a`，fo-dicom.Codecs 参考版本为 `80d8103`。RLE 样本矩阵已通过，当前阶段为经典 JPEG。经典 JPEG 达标后按 JPEG-LS、JPEG 2000 / HTJ2K 的顺序继续。

## 样本从哪里来

这次对照从本地测试目录中的真实 DICOM 提取像素。验收以这批样本为准。`radicom-bench` 自带的确定性合成图可用于额外压力测试，运行方式见 [Compression-speed benchmark](compression-benchmark.md)。

提取器为 `.interop/bench/jpeg-review/src/corpus.rs`，按以下顺序扫描各目录直属的 `.dcm` 文件，同一目录内按路径排序。

| 仓库 | 仓库相对目录 | 用途 |
|---|---|---|
| fo-dicom.Codecs | `Tests/Unit` | 单元测试文件 |
| fo-dicom.Codecs | `Tests/Acceptance` | 验收文件 |
| fo-dicom | `Tests/FO-DICOM.Tests/Test Data` | 共享测试语料 |
| radicom | `tests-data/explicit-le` | 额外候选语料 |

提取器读取第一帧 Pixel Data，接受原生小端的灰度和 RGB，要求 Bits Allocated 为 8 或 16、High Bit 等于 Bits Stored 减 1、单帧最大 32 MiB。RGB 平面数据先转成交错排列，存储容器内的未使用高位清零，并按宽、高、分配位数、存储位数、分量数、有符号标记去重。这些工作均在计时前完成。基准日志中的 `corpus <样本名> <- <文件路径>` 给出实际选中的 15 个文件，范围为 3×3 到 1972×2185。原矩阵的入选文件均来自前三个目录。RLE 的 `rle-frames.csv` 和 `rle-frames/*.raw` 记录尺寸及双方实际读到的像素。

## 如何复跑

在 Windows x64 上从仓库根目录使用 PowerShell 7 执行。`.interop/` 是 Git 忽略的本地临时目录。换机复跑时，在 `.interop/bench/jpeg-review` 恢复本地基准源码和脚本。原始 CSV 和日志按需单独归档。

在运行基准的 PowerShell 会话中配置参考源码和样本目录。输入对应仓库在当前机器上的根目录。使用自有共享语料时，按实际优先顺序调整 `$corpusDirs`，并从 `corpus` 日志核对选样。

```powershell
$bench = (Resolve-Path '.interop/bench/jpeg-review').Path
$foCodecsRoot = Read-Host 'fo-dicom.Codecs 仓库根目录'
$foDicomRoot = Read-Host 'fo-dicom 仓库根目录'
$corpusDirs = @(
    (Join-Path $foCodecsRoot 'Tests/Unit')
    (Join-Path $foCodecsRoot 'Tests/Acceptance')
    (Join-Path $foDicomRoot 'Tests/FO-DICOM.Tests/Test Data')
)
$env:DICOM_BENCH_DIRS = ($corpusDirs | ForEach-Object {
    (Resolve-Path -LiteralPath $_ -ErrorAction Stop).Path
}) -join [IO.Path]::PathSeparator
```

新机器首次使用基准时，先执行 `& "$bench/prepare-rust.ps1"` 生成 Rust 源码副本，再执行 `cargo fetch --locked --manifest-path "$bench/Cargo.toml"`，准备后续离线构建需要的依赖。

### 1. 产品正确性测试

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
cargo +1.89.0 check --workspace --all-features
git diff --check
```

针对单个 crate 可先执行 `cargo test -p radicom-codecs-jpeg` 或对应名称。工作区测试覆盖 DICOM 接入、各种采样与重启间隔、异常码流和输出界限。

### 2. RLE 与 C# 对照

```powershell
& "$bench/prepare-rust.ps1"
& "$bench/bench-rle-managed.ps1" -FoRoot $foCodecsRoot -Rounds 5
if ($LASTEXITCODE -ne 0) { throw 'RLE benchmark failed' }
```

脚本编译 fo-dicom.Codecs 的 `Codec/DicomRleCodec.cs`，通过单帧包装直接调用其 `RLEEncoder` 和 `RLEDecoder`。需要 .NET 10 SDK，以及默认 NuGet 包缓存中的 `fo-dicom` 5.2.6。可通过还原引用该版本的 .NET 项目获取该包。Rust DLL 仅用于这套测试。输出为 `pixel-rle-vs-csharp-round-1.csv` 至 `pixel-rle-vs-csharp-round-5.csv` 和汇总 `pixel-rle-vs-csharp.csv`，重跑前可归档原结果。原矩阵有 45 条结果，每个 DICOM 样本各一条编码、解码 C# 码流、解码 Rust 码流。换语料后以实际提取样本数乘以 3 核对。

### 3. JPEG、JPEG-LS、JPEG 2000 / HTJ2K 与 C/C++ 对照

本机原生桥需要 Visual Studio 的 MSBuild x64 和 C++ 工具链。`build-native.ps1` 从 fo-dicom.Codecs 原生项目构建 Release x64 桥接 DLL。JPEG 使用 libjpeg-turbo，JPEG-LS 使用 CharLS，JPEG 2000 使用 OpenJPEG，HTJ2K 使用 OpenJPH。共享像素由 Rust 提取器读取，基准命令无需 DCMTK。已有 `bridge/Dicom.Native.dll` 且参考源码未变时可直接使用现有构建。

```powershell
& "$bench/build-native.ps1" -FoNative (Join-Path $foCodecsRoot 'Native')
if ($LASTEXITCODE -ne 0) { throw 'Native build failed' }
& "$bench/prepare-rust.ps1"

Push-Location $bench
try {
    cargo build --release --offline --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust benchmark build failed' }
    $env:JPEG_BENCH_DICOM = '1'
    $env:JPEG_BENCH_OPTIMIZE_BASELINE = '1'
    # 原 15 个样本的行数。换语料后按各格式适用样本数乘以 3 核对。
    $expected = @{ classic = 138; jls = 90; j2k = 192 }

    # 当前先跑 classic。该类收口后，按 jls、j2k 顺序修改这里。
    $family = 'classic'
    $env:JPEG_BENCH_FAMILY = $family
    $files = for ($round = 1; $round -le 5; $round++) {
        $csv = "pixel-$family-verify-round-$round.csv"
        & ./target/release/jpeg-native-review.exe 20 > $csv 2> "pixel-$family-verify-round-$round.log"
        if ($LASTEXITCODE -ne 0 -or @(Import-Csv $csv).Count -ne $expected[$family]) {
            throw "Incomplete $family benchmark round $round"
        }
        $csv
    }
    & ./mean-native.ps1 -Result "pixel-$family-verify-mean.csv" -InputFiles $files
} finally {
    Pop-Location
}
```

修改产品源码后重新执行 `prepare-rust.ps1` 和 Cargo Release 构建。基准可执行文件以源码副本编译，复制源码是刷新结果的一部分。用 `20 1920x1072 P1` 可以定向测大彩色帧。格式过滤按子字符串匹配，`P1` 同时会匹配 `P14` 和 `P14SV1`。保持两边参数一致。顺序 JPEG 使用质量 90、彩色 4:4:4 和 Huffman 优化。JPEG-LS 近无损使用 `NEAR=3`。JPEG 2000 桥接器使用 6 个分辨率级别，本轮排除最小边不足 32 的帧。HTJ2K 本轮只覆盖 Bits Stored 等于 Bits Allocated 的样本。切换参考配置后重新建立基线。

### 4. 计时及达标判定

- 在同一台机器上使用 Release 构建，双方在同一进程交替计时并绑定同一逻辑核心。先用同一份原始像素编码，再由两个实现依次解码同一份参考码流和同一份 Rust 码流。
- RLE 用 5 个独立进程，每轮 21 组，3×3 样本每组批量执行 8192 次。原生对照用 5 个独立进程，原语料每轮每项 20 次。给原生基准换上超过 20,000,000 字节的帧时，JPEG-LS 和经典 JPEG 2000 的脚本会将该项每轮次数限制到 3 次。等待单轮完整结束后再启动下一轮，避免并发构建或测试干扰计时。对临界结果看各轮比例，并重复整组。
- 以算术平均计算比例。RLE 每一项均须满足 Rust/C# < 1.0，其余格式每一项均须满足 Rust/C++ ≤ 1.0。逐传输语法、逐样本、逐编解码方向判定。耗时降低百分比为 `100 × (1 − Rust 平均耗时 / 参考平均耗时)`。
- 原生桥复用调用方的输出缓冲，Rust 耗时包含向量分配。RLE 的 Rust 耗时包含基准调用边界和输出释放，C# 使用自己的托管分配。早期 `pixel-report.md` 的原生表沿用中位数，仅用于历史定位。验收使用带 `mean` 的明细文件和 RLE 五轮汇总。

## 如何验证结果正确

- 编码输入以提取后的原始像素为准。两路码流分别交给两个实现解码，每条路径均须成功，输出尺寸、精度、分量和字节数要匹配。任何日志错误或缺失行均需调查。
- RLE、P14/P14 SV1、JPEG-LS 无损、JPEG 2000 无损和 HTJ2K 无损应逐样本还原输入。RLE 基准允许 DICOM 偶数字节对齐引入一个末尾零字节。本轮 RLE 的 15 个输入还验证了 Rust 和 C# 生成的码流逐字节一致。
- JPEG-LS 近无损按无符号码流值检查最大误差 ≤ 3，并核对同码流输出。带符号 DICOM 数据跨符号边界时，按有符号样本解释可产生远大于 3 的差值。Rust 与 CharLS 同流一致，带符号 DICOM 语义仍需单独验收。
- 有损格式记录像素差个数、最大绝对差和 RMSE。顺序 JPEG 彩色同流比较在 YCbCr 空间进行，报告会留空彩色原图误差。允许不同实现的有损舍入差异时，仍要比较优化前后的 Rust 输出，防止性能改动引入新差异。此次 AVX2 IDCT 的 32 个解码输出哈希与标量版一致，AVX2 正向 DCT 的编码码流哈希也与改动前一致。后续改动涉及的样本和码流仍须重验。

## 当前结果与剩余问题

### RLE

编码已改为直接写最终帧，减少段缓冲复制。小字节平面用栈缓冲，连续平面走切片，其他平面用固定步长。五轮共 45 项均比 C# 快，每轮也都通过。平均编码耗时降低 30.2%–78.2%，两路解码耗时降低 8.6%–82.6%。

| 样本与方向 | Rust 平均耗时 | C# 平均耗时 |
|---|---:|---:|
| 3×3 RGB 编码 | 0.165 µs | 0.436 µs |
| 1972×2185 灰度编码 | 18.662 ms | 26.735 ms |
| 1972×2185 灰度解码 C# 码流 | 8.379 ms | 14.758 ms |
| 1280×1280 灰度解码 Rust 码流，差距最小 | 7.415 ms | 8.116 ms |

### 经典 JPEG，当前优化阶段

已优化 Huffman 前缀查表、位读写、DCT/IDCT、量化块缓存及 P14 预测器 1 路径。x86-64 在运行时检测 AVX2，其他目标走标量路径。最新一次完整五轮矩阵是 `pixel-classic-mean-next.csv`，包含 AVX2 正反向变换以及 16 位 P14 合并读取，早于随后合入的扫描几何和纯 DC 块写回改动。

| 格式 | 编码达标 | 解码 C/C++ 码流达标 | 解码 Rust 码流达标 |
|---|---:|---:|---:|
| P1 | 0/7 | 2/7 | 2/7 |
| P2/4 | 0/9 | 2/9 | 2/9 |
| P14 | 8/15 | 5/15 | 5/15 |
| P14 SV1 | 8/15 | 6/15 | 5/15 |

- **P1 / P2/4**。1920×1072 RGB 的 P1 最新定向复测为三轮各 20 次。编码 Rust 32.66 ms、C/C++ 20.35 ms，解码同一 C/C++ 码流 Rust 11.09 ms、C/C++ 4.65 ms。改动后这条 P1 解码路径的像素哈希和改动前一致。完整矩阵待重跑。分段采样见 `pixel-classic-profile-current.log`，这个尺寸的逐分量像素写回耗时突出。下一轮方案是在 4:4:4 整 MCU 内收齐三个分量再连续写出，复测两路码流，并保持部分块和其他采样路径的输出。此方案截至本记录尚未落地。
- **P14 / P14 SV1**。1972×2185 灰度的 P14 解码参考码流平均为 Rust 29.20 ms、C/C++ 28.63 ms，仍差 2.0%。1920×1072 RGB 的 P14 在完整矩阵中，编码为 50.91 对 37.36 ms，解码参考码流为 30.48 对 21.28 ms。继续定位 8 位 RGB 的 Huffman 读写、预测循环和帧布局。P14 SV1 独立验收。

### JPEG-LS

已去除逐像素分配、复用行缓冲，优化 Golomb 参数、位缓冲和读写。历史中位数矩阵 `pixel-jls-current.csv` 的无损、近无损各 45 项均未达标。960×540 RGB 无损解码参考码流为 Rust 9.15 ms、CharLS 2.68 ms，近无损为 10.55 对 3.43 ms。等经典 JPEG 收口后，先重建平均值矩阵，再定位逐像素上下文更新、Golomb 热点及彩色扫描路径。

### JPEG 2000 / HTJ2K

已优化经典 JPEG 2000 的 MQ、Tier-1 和整帧复制，以及 HTJ2K 码块和无损输出复制。历史中位数矩阵 `pixel-j2k-final.csv` 包含 192 项。OpenJPEG 对照中，无损编码 3/14、不可逆编码 2/14 达标，两路解码均 0/14。1972×2185 无损编码为 Rust 650.13 ms、OpenJPEG 741.84 ms，解码参考码流为 845.77 对 778.63 ms。HTJ2K 三种配置各有 36 项，均未达标。同尺寸 HTJ2K 无损的参考码流解码为 Rust 78.01 ms、OpenJPH 18.90 ms。等 JPEG-LS 收口后，重建平均值矩阵，分别定位经典 Tier-1 与 HTJ2K 码块热点，并补测 HTJ2K 的子字宽样本。

交接时的工作区测试输出位于 `.interop/bench/jpeg-review/workspace-checkpoint-tests.log`。换机或继续改动后，按上文命令重跑工作区检查，并保留各轮原始 CSV 和日志。

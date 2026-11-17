# timelapsable

`timelapsable` turns an alphabetical folder of timelapse stills into a
color-matched, flicker-reduced and crop-stabilized image sequence.

It reads Sony **ARW**, **JPEG** and **PNG** files, analyzes the whole sequence,
and writes new images to `<folder>/.timelapsable/` using the name
`<source-stem>.tlpb.jpg` (or `.tlpb.png`). `--output` writes them elsewhere.

## Features

- Automatic white-balance matching with temporal smoothing and an optional
  manual color-temperature bias.
- Automatic exposure matching with temporal smoothing, plus a manual brightness
  offset.
- Crop-based stabilization using windowed phase correlation, a smoothed camera
  path and a common safe crop rectangle.
- Per-frame analysis cache in `.meta.<source-stem>.toml`. Changing only
  brightness or white balance reuses all cached measurements and skips the
  analysis decode pass.

## Build

```bash
cargo build --release
./target/release/timelapsable --help
```

Rust 1.89 or newer is required.

## Usage

```bash
# Analyze ./images, auto white balance, auto exposure matching, stabilization on.
timelapsable -f ./images

# Encode PNG, apply manual +10% brightness, use 5600 K white balance.
timelapsable -f ./images --format .png -b 10 -w 5600

# Disable crop stabilization for a deliberately soft night sequence.
timelapsable -f ./images -s false

# Export to ./renders instead of ./images/.timelapsable, replacing old results.
timelapsable -f ./images -o ./renders --overwrite

# Ignore existing caches, re-analyze every frame and rewrite its metadata.
timelapsable -f ./images --no-cache --overwrite

# Default folder is the current directory and the default format is .jpg.
timelapsable
```

### Options

| Option | Default | Description |
| --- | --- | --- |
| `-f, --folder DIR` | `.` | Single-level input folder. No recursion. |
| `-o, --output DIR` | `<folder>/.timelapsable` | Directory for rendered images and analysis caches. Relative paths resolve against the current working directory; the directory is created when missing. |
| `--overwrite` | `false` | Replace output images that already exist. Without it, existing images are skipped and only missing frames are rendered. |
| `--no-cache` | `false` | Ignore existing analysis caches: every frame is re-analyzed and its `.meta.<stem>.toml` is regenerated. Rendered images are still skipped unless `--overwrite` is also given. |
| `--format FORMAT` | `.jpg` | Output encoding: `.jpg`/`jpeg` or `.png`. |
| `-s, --steadicam [BOOL]` | `true` | Enable crop stabilization. `-s false` disables it. |
| `-w, --white-balance WB` | auto | `auto`, `daylight`, `cloudy`, `shade`, `tungsten`, `fluorescent`, `flash`, or Kelvin in `1500..=15000` such as `5600`/`5600K`. |
| `-b, --brightness PERCENT` | no manual offset | `-100..=100`. `-100` is pure black, `100` pure white. Values outside `[-90, 90]` produce a warning. |

Long options also accept `--option=value`. The optional `-s` boolean accepts
`true/false`, `yes/no`, `on/off`, and `1/0`; `--overwrite` and `--no-cache` are
plain switches with no value.

## Output contract

For an input named `DSC0001.ARW` and the default `--output`:

```text
<folder>/.timelapsable/DSC0001.tlpb.jpg
<folder>/.timelapsable/.meta.DSC0001.toml
```

Both files live in the output directory: `<folder>/.timelapsable` by default,
or the `--output` directory. The analysis cache follows the output directory, so
a new `--output` re-analyzes the sequence once.

Input files are discovered only at the top level, filtered to
`arw/jpg/jpeg/png` case-insensitively, and processed in case-insensitive
alphabetical file-name order. Previously generated `.tlpb.*` files are skipped.

An existing output image is not replaced by default: that frame is reported as
skipped and left untouched, so an interrupted run can be resumed. Pass
`--overwrite` to re-render and replace existing images, for example after
changing brightness or white balance.

If two source files have the same stem (`A.ARW` and `A.JPG`), the run stops
with a collision error instead of overwriting one result.

## Processing pipeline

### 1. Analysis pass

Each frame is decoded, oriented according to EXIF, sampled for linear-light
channel means and luminance percentiles, and measured for Laplacian sharpness.
The analysis preview is also used for phase correlation against the previous
frame.

The result for all frames is written only after every required measurement,
including motion, is available. Writes go to a temporary file and are renamed
atomically.

### 2. White balance

The per-frame neutral estimate is a weighted gray-world estimate in linear
light, ignoring deep shadows and clipped highlights. The observed red/green and
blue/green log ratios are median filtered and smoothed into a target color
curve. Each frame's gain is the difference between that smooth target and its
own observed ratio, which removes rapid white-balance flicker while preserving
gradual lighting changes. The target is lightly anchored to the sequence median to
reduce slow drift while preserving gradual lighting changes.

`-w 6500` keeps the automatic neutral result. Lower temperatures add a warm
bias and higher temperatures add a cool bias; the bias is relative to 6500 K.

### 3. Brightness

The median luminance of every frame is smoothed over time. Each frame receives
a linear-light exposure gain that moves its median toward the smoothed curve,
removing flicker while following a gradual sunset or sunrise. The manual
`--brightness` offset is then added on top in sRGB space:

- `-100%` maps every channel to `0` (pure black).
- `0%` leaves the matched result unchanged.
- `+100%` maps every channel to `255` (pure white).

### 4. Stabilization

With stabilization enabled, consecutive frames are aligned with a Hann-windowed
phase correlation at up to 1024 pixels on the long edge. Cumulative translation
is split into a smoothed intentional camera path and a residual shake path. A
single crop size is selected so every frame's residual can be compensated
without black borders. Output dimensions therefore shrink slightly when shake
is present.

If a frame is below the minimum sharpness or the correlation peak is not
reliable, the program prints an error and exits with status **4**. Use
`-s false` to bypass stabilization deliberately.

Stabilization currently compensates translation only, not rotation, rolling
shutter or lens distortion. All frames must have identical oriented dimensions.

## Cache behavior

The cache contains the source fingerprint (name, size, nanosecond mtime), EXIF
summary, sampled color/exposure statistics, sharpness, and estimated motion
relative to the previous frame. It has a schema version, a `complete` flag and
a SHA-256 checksum over the TOML payload.

On startup:

- A complete, checksummed cache whose source and predecessor fingerprints match
  is reused without decoding that frame in the analysis pass.
- A stale, truncated, checksum-mismatched or schema-mismatched cache prints a
  warning and is regenerated.
- If only `-b` or `-w` changes, the analysis pass reports cache hits; rendering
  still decodes the source images because the pixels are needed for output.
- `--no-cache` skips the reuse check: every frame is decoded and re-analyzed,
  and each `.meta.*.toml` is rewritten. Existing rendered images are still
  skipped unless `--overwrite` is also given.

Deleting the `.meta.*.toml` files in the output directory forces full
re-analysis.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Runtime failure: bad folder, no images, decode/metadata/write error, etc. |
| `2` | Invalid command-line usage or value. |
| `4` | Stabilization failed because a frame is too blurred, too low-detail or has unreliable motion. |

## Testing

```bash
cargo test
```

The integration tests generate textured and flat PNG sequences, run the real
binary, and verify output names, the default and `--output` destinations,
existing-image skipping with `--overwrite`, cache reuse and regeneration with
`--no-cache`, crop behavior, brightness extremes, the special stabilization
exit code and argument validation.

## Third-party components

- [`rawler`](https://crates.io/crates/rawler) — Sony ARW decoding/development
  (LGPL-2.1).
- [`kamadak-exif`](https://crates.io/crates/kamadak-exif) — EXIF parsing
  (BSD-2-Clause).
- [`image`](https://crates.io/crates/image) — JPEG/PNG codecs.
- [`rustfft`](https://crates.io/crates/rustfft) — FFT used by phase correlation.

Because the ARW decoder is LGPL-2.1, redistributing a statically linked binary
requires compliance with that license. Building from source for personal use is
unaffected.

# ushoot-raw-decoder

RAW image decoder sidecar for the U-Shoot Desktop application.

## Purpose

This crate exists for one reason: to **isolate the LGPL-2.1 boundary**
around the [`rawler`](https://github.com/dnglab/dnglab) crate.

The U-Shoot Desktop main binary is proprietary commercial software.
`rawler` is licensed under LGPL-2.1, which has specific requirements
when statically linked into a proprietary binary (LGPL §6: object
files must be made available so a recipient can relink with a
modified version of the library).

To avoid the operational burden of conserving and distributing `.rlib`
archives for every release, U-Shoot SAS chose to **physically separate**
the rawler-dependent code into this standalone binary. The main
U-Shoot app talks to it over IPC (CLI args, temp files, stdout JSON).

## License

**LGPL-2.1-only**, because this binary statically links rawler. The
source code of this crate is published with U-Shoot Desktop, which
satisfies §6 by construction: any recipient can rebuild this binary
with a modified `rawler` using the sources here and the upstream
[dnglab repository](https://github.com/dnglab/dnglab).

See `legal/lgpl-rawler-compliance.md` at the repository root for the
full compliance rationale.

## CLI surface

```
ushoot-raw-decoder decode     --input <path> --output <path>
ushoot-raw-decoder dimensions --input <path>
ushoot-raw-decoder exif       --input <path>
```

- `decode` writes a packed binary blob to `<output>`: a 16-byte header
  (magic `USRD`, format version, channels, width, height) followed by
  RGB8 sRGB pixels. The main app reads this back into a `Vec<u8>` and
  applies any user-side post-processing (presets, etc.).
- `dimensions` writes `{"width": N, "height": M}` to stdout (JSON).
- `exif` writes a JSON object with RAW-level metadata (vendor, model,
  orientation, white-balance coefficients) to stdout.

Exit code is non-zero on any failure; an error message is written to
stderr.

## Build

```sh
cargo build --release
```

The produced binary is named `ushoot-raw-decoder` on Unix platforms
and `ushoot-raw-decoder.exe` on Windows. Tauri's `externalBin`
mechanism expects the binary to be staged into
`frontend/src-tauri/binaries/ushoot-raw-decoder-<target-triple>[.exe]`
before `cargo tauri build`. The release CI workflow handles this
staging automatically.

## Versioning

This crate is versioned **independently** of the main U-Shoot
application. The version follows roughly the U-Shoot major/minor
cycle so that binaries shipped together are easy to identify.

## Limitations

- Only `ThreeColor` and `Monochrome` rawler `Intermediate` variants
  are supported. Exotic four-channel sensors (CMYG, etc.) will
  produce a clean error.
- The colour temperature in Kelvin is **not** computed here — the
  main app does that from the raw `wb_coeffs` returned by the `exif`
  subcommand, because the McCamy formula is U-Shoot business logic
  that we keep on the proprietary side.

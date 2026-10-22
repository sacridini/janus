# Building Janus from source

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

Janus is a Rust workspace (`crates/`, see [Architecture](architecture.md)). It needs
[Rust](https://rustup.rs) 1.90 or later and GDAL, e.g. from a conda-forge environment:

```
conda create -n geo -c conda-forge gdal libgdal-netcdf libgdal-hdf5 numpy
```

(the two plugins read NetCDF and HDF5; numpy and GDAL's Python bindings are only for
`tools/make_selftest_data.py`, which makes the self-tests' inputs). Everything else, from the
interface (egui, wgpu) to Zeit's methods, comes from crates.io and the workspace. Zeit is part of
the program: no Python runs at any time.

## Windows

`gdal-sys` links GDAL's import library under the name the OSGeo builds give it, `gdal_i.lib`;
conda-forge calls it `gdal.lib`. Copy it once, and tell Cargo where GDAL is in
`.cargo/config.toml` (not committed; `.cargo/config.example.toml` is the template):

```powershell
mkdir .cargo\gdal; copy <conda-env>\Library\lib\gdal.lib .cargo\gdal\gdal_i.lib
copy .cargo\config.example.toml .cargo\config.toml   # then set GDAL_INCLUDE_DIR and GDAL_VERSION
cargo build --release
$env:PATH = "<conda-env>\Library\bin;$env:PATH"      # gdal.dll and its dependencies
.\target\release\janus.exe <series>
```

The program finds PROJ's and GDAL's data next to the GDAL on `PATH` (a development environment) or
next to itself (a package).

## Linux and macOS

pkg-config finds GDAL in the environment; the programs built need its libraries at run time:

```bash
conda activate geo
export PKG_CONFIG_PATH="$CONDA_PREFIX/lib/pkgconfig"
export RUSTFLAGS="-C link-arg=-Wl,-rpath,$CONDA_PREFIX/lib"   # or LD_LIBRARY_PATH / DYLD_FALLBACK_LIBRARY_PATH
cargo build --release
./target/release/janus <series>
```

The map draws through wgpu: Vulkan on Linux (any driver; Mesa's lavapipe without a GPU), Metal on
macOS, Direct3D 12 on Windows (`WGPU_BACKEND` chooses another).

## Tests

```bash
cargo fmt --all --check
cargo clippy --release --workspace --all-targets -- -D warnings
cargo test --release --workspace
python tools/make_selftest_data.py td           # the self-tests' inputs
janus --selftest-zeit td/serie                  # every Zeit tool, pixel and raster, no window
janus --selftest-zeit td/landsat                # the multiband ones (CCDC, CODED, NDFI, Tmask)
janus --selftest-validation                     # the estimators against Olofsson et al. (2014)
janus --selftest-embeddings                     # the embedding kernels and PCA
cd td && janus --selftest-ui A B C D E F G      # the interface, frame by frame (a window)
```

The UI self-test's *Copy Python* stages run the code they copy with a Python that has Zeit and
rasterio (`JANUS_PYTHON`, else `python`); without one they are skipped.

`janus --profile-tour <series>` times a fixed tour of a series (the overview, cursor series, every
date, the display modes, zoom and pan at full resolution, an ROI) stage by stage;
`--tour-json FILE` writes the timings as JSON (`bench/` keeps the baselines).

## Packages

```bash
python tools/package.py      # builds, then: dist/janus-<version>-setup.exe (Windows, Inno Setup 6),
                             # dist/janus-<version>-linux-x86_64.tar.xz, dist/janus-<version>-macos-arm64.dmg
```

It takes GDAL's libraries, plugins and data from the conda environment (`$CONDA_PREFIX`, or
`--prefix`): on Windows the DLLs the program imports, found by walking their import tables; on
Linux what `ldd` finds there (RPATH `$ORIGIN/lib`, set with `patchelf`); on macOS what `otool`
finds (RPATH `@executable_path/../Frameworks`, ad hoc signature). The Windows installer needs
[Inno Setup 6](https://jrsoftware.org/isinfo.php); without it the files are left in
`target/package`.

The CI (`.github/workflows/build.yml`) does all of the above on Windows, Linux and macOS and
publishes the three packages on a `v*` tag.

## The guides' screenshots

`janus --shots TOUR IN OUT` walks through a guide of the website in a window of 1680 × 1050 and
saves its screenshots in OUT: `validation` (IN from `tools/make_tutorial_data.py`), `landtrendr`,
`ccdc` and `coded` (`tools/make_guide_data.py`), `bfast` and `phenology` (an EVI series).

## Data folders

The overview and full-resolution caches, settings, layout, labels and results live in
`%LOCALAPPDATA%\Janus` (Windows), `~/.local/share/janus` (Linux) and
`~/Library/Application Support/Janus` (macOS; the overview cache in `~/Library/Caches/Janus`).
The cache files are the same as the C++ versions' (up to 0.39), which Janus 1.0 reads.

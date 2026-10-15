# Building Janus from source

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

Requirements: CMake ≥ 3.21, a C++17 compiler (Visual Studio 2022 on Windows,
GCC or Clang elsewhere) and a GDAL installation with CMake config files, e.g. a
conda-forge environment (`conda create -n geo -c conda-forge gdal libgdal-netcdf libgdal-hdf5`:
the two plugins read NetCDF and HDF5; the build copies them to `gdalplugins/`). GLFW, Dear
ImGui, ImPlot and nlohmann/json are downloaded by CMake (`FetchContent`).

## Windows

```powershell
cmake -S . -B build -G "Visual Studio 17 2022" -A x64 -DCMAKE_PREFIX_PATH="<conda-env>\Library"
cmake --build build --config Release
.\build\Release\janus.exe
```

The post-build step (`cmake/deploy_runtime.cmake`) copies `gdal.dll` and all of
its dependencies, plus `proj.db` and `share/gdal`, next to the executable, so the
build runs on double click and never picks up another GDAL from `PATH`.

Zeit runtime for local builds (downloads the embeddable Python and the pinned
wheels from `zeit_bridge/requirements.txt`; the bridge script itself is copied
on every build, so editing a tool needs no runtime rebuild):

```powershell
cmake --build build --config Release --target zeit_runtime   # -> build\Release\runtime
.\build\Release\janus.exe --selftest-zeit <series>              # end-to-end check, no window
```

To work on Zeit itself, point Janus at your own environment:
`jn --zeit-python <env>\python.exe` (or `JANUS_ZEIT_PYTHON`).

Installer (requires [Inno Setup 6](https://jrsoftware.org/isinfo.php)); it
assembles its own copy of the runtime in `build\package`:

```powershell
cmake --build build --config Release --target installer   # -> dist\janus-<version>-setup.exe
```

## Profiling with Tracy

`-DJANUS_TRACY=ON` builds in the [Tracy](https://github.com/wolfpld/tracy)
client (v0.14.1): zones on the main loop, the UI steps, the job pools (threads
`bg N`, `fg N`), GDAL reads, the overview, detail tiles, the full-resolution
cache and the OpenGL work, plus plots of the job queues and tiles
(`src/profiler.hpp`). Off (the default), the macros compile to nothing.

```powershell
cmake -S . -B build-tracy -G "Visual Studio 17 2022" -A x64 -DCMAKE_PREFIX_PATH="<conda-env>\Library" -DJANUS_TRACY=ON
cmake --build build-tracy --config Release --target janus
```

The profiler must be the same version: `windows-0.14.1.zip` from Tracy's
[releases](https://github.com/wolfpld/tracy/releases/tag/v0.14.1).

- **Live**: start `tracy-profiler.exe -a 127.0.0.1` (or click *Connect*), then
  `build-tracy\Release\janus.exe <series>`. The client collects from the start,
  so the profiler can also connect later.
- **A fixed workload**: `janus.exe --profile-tour <series>` opens the series in
  a hidden window and goes through the overview, cursor series, every date, the
  display modes, a zoom and pan at full resolution and an ROI, printing each
  stage's time (it also works without Tracy). Each stage is a span in Tracy's
  timeline. To record it to a file instead:

  ```powershell
  tracy-capture.exe -o tour.tracy -a 127.0.0.1    # waits for the program
  $env:TRACY_NO_EXIT = 1; .\build-tracy\Release\janus.exe --profile-tour <series>
  tracy-csvexport.exe tour.tracy > zones.csv       # or open tour.tracy in tracy-profiler
  ```

## Linux

Everything (compiler, GDAL, X11/OpenGL headers) can come from one conda-forge
environment, so nothing needs to be installed system-wide:

```bash
conda create -n janus-linux -c conda-forge cmake ninja cxx-compiler c-compiler pkg-config gdal libgdal-netcdf libgdal-hdf5 \
    xorg-libx11 xorg-libxrandr xorg-libxinerama xorg-libxcursor xorg-libxi xorg-libxext libgl-devel
conda activate janus-linux
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH="$CONDA_PREFIX" -DGLFW_BUILD_WAYLAND=OFF
cmake --build build
cmake --build build --target zeit_runtime     # -> build/runtime (python-build-standalone + Zeit)
./build/janus --selftest-zeit <series>
cmake --build build --target package_linux    # -> dist/janus-<version>-linux-x86_64.tar.xz
```

(`-DGLFW_BUILD_WAYLAND=OFF` skips GLFW's native Wayland backend, which needs
`wayland-scanner`; on Wayland desktops Janus runs through XWayland.)

## macOS (Apple Silicon)

Builds with the Xcode command line tools (AppleClang) and GDAL from a conda
environment; tested on macOS 26, UI and Zeit self-tests pass:

```bash
conda create -n geo -c conda-forge gdal libgdal-netcdf libgdal-hdf5
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH="$HOME/miniconda3/envs/geo" \
    -DPython3_EXECUTABLE="$HOME/miniconda3/envs/geo/bin/python3"
cmake --build build -j
cmake --build build --target zeit_runtime     # -> build/runtime (python-build-standalone + Zeit)
./build/janus --selftest-zeit <series>
ln -s "$PWD/build/janus" ~/.local/bin/jn       # jn from any terminal (runtime/ is found through the link)
cmake --build build --target package_macos    # -> dist/janus-<version>-macos-arm64.dmg (Janus.app)
```

It renders through Metal (`JANUS_RENDERER=METAL`, the default on macOS): the cube
and its statistics live in shared-memory buffers that the shaders read directly,
so uploading a date is a copy in RAM, and the shaders are compiled at startup
(the command line tools have no offline Metal compiler; the system caches them).
The cube buffer *is* the overview's page-aligned array (`newBufferWithBytesNoCopy`):
nothing is uploaded and the cube exists once in RAM, so `--budget` is the
memory the overview takes in total (with OpenGL it is taken on the GPU and again
in RAM, which on Apple Silicon is the same memory). With a 1 GB cube on an M4:
1.2 GB footprint (OpenGL: 3.8 GB), loaded from the cache in 0.2 s.
`-DJANUS_RENDERER=GL` builds the OpenGL path instead, e.g. to compare both.

The build tree links GDAL from the conda environment through an absolute RPATH,
so it breaks if that environment is removed; `package_macos` makes the
self-contained app (`cmake/package_macos.cmake`: the library chain in
`Contents/Frameworks`, data and runtime in `Contents/Resources`, ad hoc
signature). The Zeit runtime re-signs (ad hoc) the native libraries it
strips: on Apple Silicon a binary with an invalid signature is killed on load.

On a Retina display the map is drawn at the density of the screen it is on
(2 pixels per point), and full-resolution tiles are chosen by screen pixels.
The overview cache lives in `~/Library/Caches/Janus` (purgeable, skipped by Time
Machine); the rest of the data in `~/Library/Application Support/Janus`.

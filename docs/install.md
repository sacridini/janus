# Installing Janus

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

## Windows

Run `janus-<version>-setup.exe`. It installs per user (no administrator prompt),
bundles GDAL and a private Python runtime with Zeit (no Python or conda needed,
nothing is added to your Python installations), adds a Start menu entry and,
optionally:

- adds Janus to `PATH`, so you can call it as `jn` from any terminal;
- adds **Open in Janus** to the right-click menu of folders and `.tif` files;
- creates a desktop shortcut.

## Linux x86_64

Extract `janus-<version>-linux-x86_64.tar.xz` anywhere and run `jn` from that
folder (e.g. `~/apps/janus-<version>/jn serie.tif`, or link it into `~/.local/bin`).
The archive is portable: it carries GDAL and its libraries, the PROJ/GDAL data
and the private Python runtime with Zeit; only glibc, OpenGL and X11 come from
the system (tested on Ubuntu 24.04). `janus.desktop` and `janus.png` are included
for a menu entry. File dialogs use `zenity` or `kdialog` when installed; the
Files panel works without them. Data and caches live in `~/.local/share/janus`.
The basemap's tiles are downloaded over https with the system's CA certificates
(`/etc/ssl/certs/ca-certificates.crt` and the like; the archive carries a copy
in `share/ssl` for systems without one; `CURL_CA_BUNDLE` overrides both).

## macOS (Apple Silicon)

Open `janus-<version>-macos-arm64.dmg` and drag **Janus** into **Applications**.
The app carries GDAL and its libraries, the PROJ/GDAL data and the private
Python runtime with Zeit (macOS 15 or later). Folders and rasters can be opened
from the Finder (right click → Open With → Janus), by dropping them on the
window or the Dock icon, or from a terminal after linking the program once:

```
ln -s /Applications/Janus.app/Contents/MacOS/janus ~/.local/bin/jn   # or /usr/local/bin
```

The app is not signed with an Apple Developer ID: the first time, macOS says it
cannot verify the developer. Open it once with right click → Open (or allow it
in System Settings → Privacy & Security), or clear the download's quarantine
flag with `xattr -dr com.apple.quarantine /Applications/Janus.app`. Data lives in
`~/Library/Application Support/Janus`, the overview cache in `~/Library/Caches/Janus`.

Installers and packages are built from this repository (see
[Building](building.md)); every tagged version publishes the three of
them on the [Releases](https://github.com/sacridini/janus/releases) page (GitHub
Actions, `.github/workflows/build.yml`).

//! Windows: the program's icon and version in Explorer and the taskbar (the C++'s resources/janus.rc.in), and the
//! GDAL it is built against put beside it (its DLLs, plugins and data), so that any janus.exe the build makes runs
//! with its own GDAL, as the installed one does, never with whatever GDAL comes first on the PATH.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../../resources/janus.ico");
    // the GDAL built against (CI's GDAL_VERSION, else the build environment's gdalinfo): at start, a GDAL of another
    // version loaded is reported
    println!("cargo:rerun-if-env-changed=GDAL_VERSION");
    let version = std::env::var("GDAL_VERSION").ok().or_else(|| {
        let out = std::process::Command::new("gdalinfo").arg("--version").output().ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        Some(text.split_whitespace().nth(1)?.trim_end_matches(',').to_string())
    });
    if let Some(v) = version {
        println!("cargo:rustc-env=JANUS_GDAL_BUILT={v}");
    }
    // the build script runs on the host: a Windows host building for Windows
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut r = winresource::WindowsResource::new();
        r.set_icon("../../resources/janus.ico")
            .set("FileDescription", "Janus - raster time series analysis")
            .set("ProductName", "Janus")
            .set("InternalName", "janus");
        if let Err(e) = r.compile() {
            println!("cargo:warning=the program's icon and version were not embedded: {e}");
        }
        if let Err(e) = gdal_beside_exe() {
            println!("cargo:warning=GDAL was not put beside janus.exe ({e}): it will load the PATH's");
        }
    }
}

/// The GDAL of GDAL_INCLUDE_DIR (<prefix>/Library/include, conda-forge's layout, as .cargo/config.toml and CI set
/// it) copied into the profile's folder (target/<profile>, where janus.exe is linked): gdal.dll and every DLL it
/// loads from that prefix (tools/package.py's walk of the import tables), the netCDF and HDF5 plugins, PROJ's
/// database and GDAL's data. main.rs's configure_data finds them there as it finds the installed program's. A file
/// already there with the same size is left alone.
fn gdal_beside_exe() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed=GDAL_INCLUDE_DIR");
    let include = PathBuf::from(std::env::var("GDAL_INCLUDE_DIR").map_err(|_| "GDAL_INCLUDE_DIR is not set")?);
    let library = include.parent().ok_or("GDAL_INCLUDE_DIR has no parent")?.to_path_buf();
    let bin = library.join("bin");
    let gdal = bin.join("gdal.dll");
    if !gdal.exists() {
        return Err(format!("no {}", gdal.display()));
    }
    println!("cargo:rerun-if-changed={}", gdal.display());
    // OUT_DIR is target/<profile>/build/janus-app-<hash>/out
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let exe_dir = out.ancestors().nth(3).ok_or("no profile folder")?.to_path_buf();
    let env_root = library.parent().map(Path::to_path_buf);
    let find =
        |name: &str| std::iter::once(bin.clone()).chain(env_root.clone()).map(|d| d.join(name)).find(|p| p.exists());
    let plugins: Vec<PathBuf> = ["gdal_netCDF.dll", "gdal_HDF5.dll"]
        .iter()
        .map(|n| library.join("lib").join("gdalplugins").join(n))
        .filter(|p| p.exists())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut todo: Vec<PathBuf> = std::iter::once(gdal).chain(plugins.iter().cloned()).collect();
    let mut dlls = Vec::new();
    while let Some(f) = todo.pop() {
        if f.parent() == Some(bin.as_path()) || env_root.as_deref() == f.parent() {
            dlls.push(f.clone());
        }
        for name in pe_imports(&f) {
            let key = name.to_ascii_lowercase();
            if key.starts_with("api-ms-") || key.starts_with("ext-ms-") || !seen.insert(key) {
                continue;
            }
            if let Some(p) = find(&name) {
                todo.push(p);
            }
        }
    }
    for d in &dlls {
        copy_if_changed(d, &exe_dir.join(d.file_name().unwrap()))?;
    }
    for p in &plugins {
        copy_if_changed(p, &exe_dir.join("gdalplugins").join(p.file_name().unwrap()))?;
    }
    let share = library.join("share");
    copy_if_changed(&share.join("proj").join("proj.db"), &exe_dir.join("share").join("proj").join("proj.db"))?;
    for e in std::fs::read_dir(share.join("gdal")).map_err(|e| e.to_string())?.flatten() {
        if e.path().is_file() {
            copy_if_changed(&e.path(), &exe_dir.join("share").join("gdal").join(e.file_name()))?;
        }
    }
    Ok(())
}

fn copy_if_changed(from: &Path, to: &Path) -> Result<(), String> {
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
    if size(from).is_some() && size(from) == size(to) {
        return Ok(());
    }
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("{}: {e}", to.display()))
}

/// The DLLs a PE file imports: its import table and its delay-load table (tools/package.py's pe_imports).
fn pe_imports(path: &Path) -> Vec<String> {
    let Ok(data) = std::fs::read(path) else { return vec![] };
    let u16_at = |o: usize| data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let Some(pe) = u32_at(0x3C) else { return vec![] };
    if data.get(pe..pe + 4) != Some(b"PE\0\0") {
        return vec![];
    }
    let coff = pe + 4;
    let (Some(nsec), Some(opt_size)) = (u16_at(coff + 2), u16_at(coff + 16)) else { return vec![] };
    let opt = coff + 20;
    let dirs = opt + if u16_at(opt) == Some(0x20B) { 112 } else { 96 };
    let sections: Vec<(usize, usize, usize, usize)> = (0..nsec)
        .filter_map(|i| {
            let s = opt + opt_size + 40 * i;
            Some((u32_at(s + 8)?, u32_at(s + 12)?, u32_at(s + 16)?, u32_at(s + 20)?))
        })
        .collect();
    let offset = |rva: usize| {
        sections
            .iter()
            .find(|&&(vsize, vaddr, rawsize, _)| vaddr <= rva && rva < vaddr + vsize.max(rawsize))
            .map(|&(_, vaddr, _, rawptr)| rva - vaddr + rawptr)
    };
    let name_at = |rva: usize| {
        let o = offset(rva)?;
        let end = o + data.get(o..)?.iter().position(|&b| b == 0)?;
        Some(String::from_utf8_lossy(&data[o..end]).into_owned())
    };
    let mut names = Vec::new();
    // (directory entry, descriptor size, where the name's RVA is in it)
    for (entry, size, at) in [(1, 20, 12), (13, 32, 4)] {
        let Some(rva) = u32_at(dirs + 8 * entry).filter(|&r| r != 0) else { continue };
        let mut o = offset(rva);
        while let Some(p) = o.filter(|&p| p + size <= data.len()) {
            match u32_at(p + at) {
                Some(0) | None => break,
                Some(name_rva) => names.extend(name_at(name_rva)),
            }
            o = Some(p + size);
        }
    }
    names
}

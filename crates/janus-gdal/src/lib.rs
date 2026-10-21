//! The parts of GDAL's C API that the `gdal` crate does not cover, declared by hand (the prebuilt bindings of
//! `gdal-sys` lack them). Linked to the same GDAL as the `gdal` crate.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

// gdal-sys links GDAL; nothing of it is used here but the link.
use gdal_sys as _;

type CompressionFunc = unsafe extern "C" fn(
    input: *const c_void,
    input_size: usize,
    output: *mut *mut c_void,
    output_size: *mut usize,
    options: *const *const c_char,
    user_data: *mut c_void,
) -> bool;

/// cpl_compressor.h's CPLCompressor (structure version 1).
#[repr(C)]
struct CplCompressor {
    struct_version: c_int,
    id: *const c_char,
    kind: c_int,
    metadata: *const *const c_char,
    func: Option<CompressionFunc>,
    user_data: *mut c_void,
}

unsafe extern "C" {
    fn CPLGetCompressor(id: *const c_char) -> *const CplCompressor;
    fn CPLGetDecompressor(id: *const c_char) -> *const CplCompressor;
}

/// One of GDAL's registered compressors and its decompressor (cpl_compressor.h): "zstd", "zlib", "lz4"... The
/// full-resolution cache uses them, so the Rust writes the bytes the C++ writes, with the same libraries.
#[derive(Clone, Copy)]
pub struct Codec {
    comp: &'static CplCompressor,
    decomp: &'static CplCompressor,
}

// The registry's entries live as long as GDAL and their functions are thread-safe (GDAL's own use).
unsafe impl Send for Codec {}
unsafe impl Sync for Codec {}

impl Codec {
    /// The codec of this name, if this GDAL has both directions of it.
    pub fn get(name: &str) -> Option<Codec> {
        let id = CString::new(name).ok()?;
        // SAFETY: GDAL returns a pointer into its registry, or null.
        let (c, d) = unsafe { (CPLGetCompressor(id.as_ptr()), CPLGetDecompressor(id.as_ptr())) };
        if c.is_null() || d.is_null() {
            return None;
        }
        // SAFETY: non-null entries of the registry, never freed while GDAL is loaded.
        let (comp, decomp) = unsafe { (&*c, &*d) };
        (comp.func.is_some() && decomp.func.is_some()).then_some(Codec { comp, decomp })
    }

    pub fn name(&self) -> String {
        // SAFETY: the registry's id is a C string.
        unsafe { CStr::from_ptr(self.comp.id) }.to_string_lossy().into_owned()
    }

    /// Compresses `input` into `out` (resized to what was written); `options` as "KEY=VALUE" (e.g. "LEVEL=1").
    /// False if the output would not fit in `capacity` bytes or the codec failed.
    pub fn compress(&self, input: &[u8], out: &mut Vec<u8>, capacity: usize, options: &[&str]) -> bool {
        let opts: Vec<CString> = options.iter().filter_map(|o| CString::new(*o).ok()).collect();
        let mut ptrs: Vec<*const c_char> = opts.iter().map(|o| o.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        out.resize(capacity, 0);
        let mut dst = out.as_mut_ptr() as *mut c_void;
        let mut size = capacity;
        // SAFETY: the input and the output buffer of `capacity` bytes outlive the call; GDAL writes at most `size`
        // bytes into `dst` (given, it does not allocate) and reports how many.
        let ok = unsafe {
            (self.comp.func.unwrap())(
                input.as_ptr() as *const c_void,
                input.len(),
                &mut dst,
                &mut size,
                ptrs.as_ptr(),
                self.comp.user_data,
            )
        };
        out.truncate(if ok { size } else { 0 });
        ok && size > 0
    }

    /// Decompresses `input` into `out`, which must be its exact decompressed size.
    pub fn decompress(&self, input: &[u8], out: &mut [u8]) -> bool {
        let mut dst = out.as_mut_ptr() as *mut c_void;
        let mut size = out.len();
        let none: [*const c_char; 1] = [std::ptr::null()];
        // SAFETY: as in compress: GDAL writes at most out.len() bytes into the caller's buffer.
        let ok = unsafe {
            (self.decomp.func.unwrap())(
                input.as_ptr() as *const c_void,
                input.len(),
                &mut dst,
                &mut size,
                none.as_ptr(),
                self.decomp.user_data,
            )
        };
        ok && size == out.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let codec = Codec::get("zstd").or_else(|| Codec::get("zlib")).expect("a codec");
        let input: Vec<u8> = (0..20000u32).map(|i| (i % 251) as u8 / 4).collect();
        let mut packed = Vec::new();
        assert!(codec.compress(&input, &mut packed, input.len() + 1024, &["LEVEL=1"]));
        assert!(packed.len() < input.len());
        let mut back = vec![0u8; input.len()];
        assert!(codec.decompress(&packed, &mut back));
        assert_eq!(back, input);
        assert!(Codec::get("no such codec").is_none());
    }
}

//! The parts of GDAL's C API that the `gdal` crate does not cover, declared by hand (the prebuilt bindings of
//! `gdal-sys` lack them). Linked to the same GDAL as the `gdal` crate.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

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

/// cpl_http.h's CPLHTTPResult.
#[repr(C)]
struct CplHttpResult {
    status: c_int,
    content_type: *mut c_char,
    err_buf: *mut c_char,
    data_len: c_int,
    data_alloc: c_int,
    data: *mut u8,
    headers: *mut *mut c_char,
    mime_part_count: c_int,
    mime_parts: *mut c_void,
}

unsafe extern "C" {
    fn CPLHTTPFetch(url: *const c_char, options: *const *const c_char) -> *mut CplHttpResult;
    fn CPLHTTPDestroyResult(result: *mut CplHttpResult);
}

/// An HTTP request through GDAL's libcurl (its certificates, proxy and retry settings): a GET, or a POST of `post`
/// (JSON). `options` are CPLHTTPFetch's ("TIMEOUT=30", "HEADERS=..."). The body, or why there is none (curl's
/// error, the HTTP code and the start of the server's answer).
pub fn http_fetch(url: &str, post: Option<&str>, options: &[&str]) -> Result<Vec<u8>, String> {
    let mut opts: Vec<CString> = options.iter().filter_map(|o| CString::new(*o).ok()).collect();
    if let Some(body) = post {
        opts.push(CString::new(format!("POSTFIELDS={body}")).map_err(|e| e.to_string())?);
        opts.push(CString::new("HEADERS=Content-Type: application/json\r\nAccept: application/json").unwrap());
    }
    let mut ptrs: Vec<*const c_char> = opts.iter().map(|o| o.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    let c_url = CString::new(url).map_err(|e| e.to_string())?;
    // SAFETY: C strings alive for the call; GDAL returns a result we own and free once.
    unsafe {
        let r = CPLHTTPFetch(c_url.as_ptr(), ptrs.as_ptr());
        if r.is_null() {
            return Err(format!("{url}: no answer"));
        }
        let res = &*r;
        let body = if res.data.is_null() || res.data_len <= 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(res.data, res.data_len as usize).to_vec()
        };
        let err = (!res.err_buf.is_null()).then(|| CStr::from_ptr(res.err_buf).to_string_lossy().into_owned());
        CPLHTTPDestroyResult(r);
        match err {
            Some(e) => {
                let text = String::from_utf8_lossy(&body[..body.len().min(300)]).trim().to_string();
                Err(if text.is_empty() { e } else { format!("{e}: {text}") })
            }
            None => Ok(body),
        }
    }
}

/// A configuration option for the paths under `prefix` only (VSISetPathSpecificOption): credentials of a
/// bucket, GDAL_DISABLE_READDIR_ON_OPEN for a host. `None` removes it.
pub fn set_path_option(prefix: &str, key: &str, value: Option<&str>) {
    let (Ok(p), Ok(k)) = (CString::new(prefix), CString::new(key)) else { return };
    let v = value.and_then(|v| CString::new(v).ok());
    // SAFETY: C strings alive for the call; GDAL copies them.
    unsafe {
        gdal_sys::VSISetPathSpecificOption(p.as_ptr(), k.as_ptr(), v.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()))
    }
}

/// The size and modification time (seconds) of a file in one of GDAL's virtual file systems (VSIStatExL with
/// VSI_STAT_SIZE_FLAG), None if it cannot be read. GDAL's VSIStatBufL is the C library's stat64 on Linux and stat
/// on macOS, which the prebuilt bindings of gdal-sys leave opaque: libc's; on Windows the bindings' _stat64.
pub fn vsi_stat(path: &str) -> Option<(u64, i64)> {
    #[cfg(target_os = "linux")]
    type Stat = libc::stat64;
    #[cfg(all(unix, not(target_os = "linux")))]
    type Stat = libc::stat;
    #[cfg(not(unix))]
    type Stat = gdal_sys::VSIStatBufL;
    let c = CString::new(path).ok()?;
    // SAFETY: a C string and a zeroed struct of the layout GDAL fills.
    unsafe {
        let mut st: Stat = std::mem::zeroed();
        (gdal_sys::VSIStatExL(c.as_ptr(), (&raw mut st).cast(), 0x8) == 0)
            .then_some((st.st_size as u64, st.st_mtime as i64))
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

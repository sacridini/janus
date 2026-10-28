//! Blosc (version 1 frames), for a GDAL built without it (GISInternals' builds, for one): TESSERA's chunks are
//! blosc over zstd with a byte or a bit shuffle. The frame is read here, the zstd streams by GDAL's zstd codec, the
//! shuffles undone here. Only zstd (and blocks stored as they are) inside: the other compressors of blosc are not
//! TESSERA's.
//!
//! A frame (c-blosc 1.x, blosc.c): a 16-byte header (version, its compressor's version, flags, the type's size,
//! then the bytes it holds, the block size and the frame's size, little-endian), then each block's offset, then
//! the blocks; a block is cut in `typesize` streams (unless flag 0x10 says not, or it is the last, shorter block,
//! or its elements are too few), each a 4-byte size and its compressed bytes (stored as they are when the size is
//! the stream's own). The flags: 0x1 byte shuffle, 0x2 the whole frame stored as it is, 0x4 bit shuffle, the top
//! three bits the compressor (4: zstd).

/// The blosc of this GDAL, or the frames read here with its zstd.
pub(crate) enum Blosc {
    Gdal(janus_gdal::Codec),
    Own(janus_gdal::Codec),
}

impl Blosc {
    pub(crate) fn get() -> Result<Blosc, String> {
        if let Some(c) = janus_gdal::Codec::get("blosc") {
            return Ok(Blosc::Gdal(c));
        }
        janus_gdal::Codec::get("zstd")
            .map(Blosc::Own)
            .ok_or_else(|| "this GDAL has neither blosc nor zstd (TESSERA's compression)".to_string())
    }

    /// `input` into `out`, which must be its exact decompressed size.
    pub(crate) fn decompress(&self, input: &[u8], out: &mut [u8]) -> bool {
        match self {
            Blosc::Gdal(c) => c.decompress(input, out),
            Blosc::Own(zstd) => decompress(input, out, &|i, o| zstd.decompress(i, o)).is_ok(),
        }
    }
}

const MAX_SPLITS: usize = 16;
const MIN_BUFFERSIZE: usize = 128;

fn u32_at(b: &[u8], at: usize) -> Result<usize, String> {
    let v = b.get(at..at + 4).ok_or("blosc: the frame is cut short")?;
    Ok(u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as usize)
}

/// A blosc 1 frame into `out` (its exact size), the zstd streams through `zstd` (input, output of its exact size).
pub(crate) fn decompress(input: &[u8], out: &mut [u8], zstd: &dyn Fn(&[u8], &mut [u8]) -> bool) -> Result<(), String> {
    if input.len() < 16 {
        return Err("blosc: no header".into());
    }
    let (version, flags, typesize) = (input[0], input[2], usize::from(input[3]).max(1));
    let (nbytes, blocksize, cbytes) = (u32_at(input, 4)?, u32_at(input, 8)?, u32_at(input, 12)?);
    if version == 0 || version > 2 {
        return Err(format!("blosc: format version {version} not known"));
    }
    if nbytes != out.len() || cbytes > input.len() {
        return Err(format!("blosc: {nbytes} bytes in the frame, {} expected", out.len()));
    }
    if flags & 0x2 != 0 {
        // stored as it is
        out.copy_from_slice(input.get(16..16 + nbytes).ok_or("blosc: the frame is cut short")?);
        return Ok(());
    }
    if nbytes == 0 {
        return Ok(());
    }
    let compressor = flags >> 5;
    if compressor != 4 {
        let name = ["blosclz", "lz4", "snappy", "zlib", "zstd"].get(usize::from(compressor)).copied().unwrap_or("?");
        return Err(format!("blosc: its {name} is not read here (zstd only)"));
    }
    if blocksize == 0 {
        return Err("blosc: a block size of 0".into());
    }
    let nblocks = nbytes.div_ceil(blocksize);
    let dont_split = flags & 0x10 != 0;
    let mut tmp = vec![0u8; blocksize];
    for b in 0..nblocks {
        let start = u32_at(input, 16 + 4 * b)?;
        let bsize = blocksize.min(nbytes - b * blocksize);
        let leftover = bsize < blocksize;
        let nsplits = if !dont_split && !leftover && typesize <= MAX_SPLITS && bsize / typesize >= MIN_BUFFERSIZE {
            typesize
        } else {
            1
        };
        let neblock = bsize / nsplits;
        let mut at = start;
        let block = &mut tmp[..bsize];
        for s in 0..nsplits {
            let n = u32_at(input, at)?;
            at += 4;
            let src = input.get(at..at + n).ok_or("blosc: a stream is cut short")?;
            let dst = &mut block[s * neblock..(s + 1) * neblock];
            if n == neblock {
                dst.copy_from_slice(src);
            } else if !zstd(src, dst) {
                return Err("blosc: a zstd stream could not be decompressed".into());
            }
            at += n;
        }
        let dest = &mut out[b * blocksize..b * blocksize + bsize];
        if flags & 0x4 != 0 && typesize > 0 {
            bit_unshuffle(block, dest, typesize);
        } else if flags & 0x1 != 0 && typesize > 1 {
            byte_unshuffle(block, dest, typesize);
        } else {
            dest.copy_from_slice(block);
        }
    }
    Ok(())
}

/// Byte shuffle undone: the block holds byte j of every element, then byte j + 1...; the bytes past the last whole
/// element as they are.
fn byte_unshuffle(src: &[u8], dst: &mut [u8], typesize: usize) {
    let n = src.len() / typesize;
    for j in 0..typesize {
        let stream = &src[j * n..(j + 1) * n];
        for (i, &v) in stream.iter().enumerate() {
            dst[i * typesize + j] = v;
        }
    }
    let whole = n * typesize;
    dst[whole..].copy_from_slice(&src[whole..]);
}

/// Bit shuffle undone (bitshuffle's layout): row 8 j + k holds bit k of byte j of every element, element i at bit
/// i % 8 of the row's byte i / 8. Done on the whole elements in multiples of 8 (c-blosc's); the rest as it is.
fn bit_unshuffle(src: &[u8], dst: &mut [u8], typesize: usize) {
    let size = src.len() / typesize;
    if !size.is_multiple_of(8) {
        dst.copy_from_slice(src);
        return;
    }
    let row = size / 8;
    dst.fill(0);
    for j in 0..typesize {
        for k in 0..8 {
            let bits = &src[(j * 8 + k) * row..(j * 8 + k + 1) * row];
            for (ib, &byte) in bits.iter().enumerate() {
                if byte == 0 {
                    continue;
                }
                for m in 0..8 {
                    if byte >> m & 1 != 0 {
                        dst[(ib * 8 + m) * typesize + j] |= 1 << k;
                    }
                }
            }
        }
    }
    let whole = size * typesize;
    dst[whole..].copy_from_slice(&src[whole..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames GDAL's own blosc wrote (when this GDAL has it: conda-forge's does), read back here.
    #[test]
    fn frames_as_gdal_writes_them() {
        let (Some(blosc), Some(zstd)) = (janus_gdal::Codec::get("blosc"), janus_gdal::Codec::get("zstd")) else {
            eprintln!("no blosc or zstd in this GDAL: skipped");
            return;
        };
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        };
        for (typesize, shuffle, n) in [
            (1, "BIT", 131072),
            (4, "BYTE", 4096),
            (4, "BIT", 4096),
            (1, "BIT", 1000),
            (4, "BYTE", 70000 * 4 + 3),
            (2, "NOSHUFFLE", 5000),
            (8, "BIT", 64 * 1024 + 40),
        ] {
            // smooth values (as embeddings are), compressible, with some noise
            let data: Vec<u8> = (0..n).map(|i| ((i / 7) as u8).wrapping_add(next() & 3)).collect();
            let opts = [
                "CNAME=zstd".to_string(),
                "CLEVEL=3".to_string(),
                format!("SHUFFLE={shuffle}"),
                format!("TYPESIZE={typesize}"),
            ];
            let opts: Vec<&str> = opts.iter().map(String::as_str).collect();
            let mut frame = Vec::new();
            assert!(blosc.compress(&data, &mut frame, n + 4096, &opts), "{typesize} {shuffle} {n}");
            let mut out = vec![0u8; n];
            decompress(&frame, &mut out, &|i, o| zstd.decompress(i, o)).unwrap();
            assert!(out == data, "typesize {typesize}, {shuffle}, {n} bytes: differ");
        }
    }
}

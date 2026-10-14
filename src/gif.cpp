#include "gif.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstring>
#include <filesystem>
#include <iterator>

namespace fs = std::filesystem;

namespace {

double msSince(std::chrono::steady_clock::time_point t) {
    return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t).count();
}

inline uint32_t rgbOf(const unsigned char* p) { return uint32_t(p[0]) << 16 | uint32_t(p[1]) << 8 | p[2]; }
// Histogram bins: 6 bits per channel (5 bits merged neighbouring colours of
// smooth colour maps into visible bands).
inline int binOf(int r, int g, int b) { return (r >> 2) << 12 | (g >> 2) << 6 | (b >> 2); }

// Up to 256 distinct colours: the palette and the indices, in one pass (an
// open-addressing table of 1024 slots); false as soon as a 257th appears.
bool exactPalette(const unsigned char* rgba, size_t n, GifPalette& pal, std::vector<uint8_t>& index) {
    constexpr uint32_t kEmpty = 0xFFFFFFFFu;
    std::array<uint32_t, 1024> key;
    std::array<uint8_t, 1024> val{};
    key.fill(kEmpty);
    pal.n = 0;
    uint32_t last = kEmpty;
    uint8_t lastIdx = 0;
    for (size_t i = 0; i < n; ++i) {
        const uint32_t c = rgbOf(rgba + i * 4);
        if (c != last) {
            uint32_t h = (c * 2654435761u) >> 22;
            while (key[h] != kEmpty && key[h] != c) h = (h + 1) & 1023;
            if (key[h] == kEmpty) {
                if (pal.n == 256) return false;
                key[h] = c;
                val[h] = uint8_t(pal.n);
                pal.rgb[pal.n * 3] = uint8_t(c >> 16);
                pal.rgb[pal.n * 3 + 1] = uint8_t(c >> 8);
                pal.rgb[pal.n * 3 + 2] = uint8_t(c);
                ++pal.n;
            }
            last = c;
            lastIdx = val[h];
        }
        index[i] = lastIdx;
    }
    return true;
}

} // namespace

void gifQuantize(const unsigned char* rgba, int w, int h, bool dither, GifPalette& pal, std::vector<uint8_t>& index) {
    const size_t n = size_t(w) * h;
    index.resize(n);
    if (!dither && exactPalette(rgba, n, pal, index)) return;

    // Histogram on 6 bits per channel, with the sums for each bin's true mean.
    constexpr int kBins = 1 << 18;
    std::vector<uint32_t> count(kBins, 0);
    std::vector<uint64_t> sum(size_t(kBins) * 3, 0);
    for (size_t i = 0; i < n; ++i) {
        const unsigned char* p = rgba + i * 4;
        const int b = binOf(p[0], p[1], p[2]);
        ++count[b];
        sum[size_t(b) * 3] += p[0];
        sum[size_t(b) * 3 + 1] += p[1];
        sum[size_t(b) * 3 + 2] += p[2];
    }
    std::vector<int> bins;
    for (int b = 0; b < kBins; ++b)
        if (count[b]) bins.push_back(b);

    // Median cut: the box with the most pixels x its longest side is split at
    // the median of that side, until 256 boxes (or no box can be split).
    struct Box {
        int begin, end;
        uint64_t pixels;
        int axis, range;
    };
    auto chan = [](int bin, int axis) { return (bin >> (12 - 6 * axis)) & 63; };
    auto measure = [&](Box& bx) {
        int lo[3] = {63, 63, 63}, hi[3] = {0, 0, 0};
        bx.pixels = 0;
        for (int i = bx.begin; i < bx.end; ++i) {
            for (int a = 0; a < 3; ++a) {
                lo[a] = std::min(lo[a], chan(bins[i], a));
                hi[a] = std::max(hi[a], chan(bins[i], a));
            }
            bx.pixels += count[bins[i]];
        }
        bx.axis = 0;
        for (int a = 1; a < 3; ++a)
            if (hi[a] - lo[a] > hi[bx.axis] - lo[bx.axis]) bx.axis = a;
        bx.range = hi[bx.axis] - lo[bx.axis];
    };
    std::vector<Box> boxes;
    boxes.push_back({0, int(bins.size()), 0, 0, 0});
    measure(boxes[0]);
    while (boxes.size() < 256) {
        int best = -1;
        double bestScore = 0;
        for (size_t k = 0; k < boxes.size(); ++k) {
            const double score = double(boxes[k].pixels) * boxes[k].range;
            if (boxes[k].end - boxes[k].begin > 1 && boxes[k].range > 0 && score > bestScore) {
                bestScore = score;
                best = int(k);
            }
        }
        if (best < 0) break;
        Box bx = boxes[best];
        std::sort(bins.begin() + bx.begin, bins.begin() + bx.end,
                  [&](int a, int b) { return chan(a, bx.axis) < chan(b, bx.axis); });
        uint64_t acc = 0;
        int cut = bx.begin + 1;
        for (int i = bx.begin; i < bx.end - 1; ++i) {
            acc += count[bins[i]];
            cut = i + 1;
            if (acc * 2 >= bx.pixels) break;
        }
        Box a{bx.begin, cut, 0, 0, 0}, b{cut, bx.end, 0, 0, 0};
        measure(a);
        measure(b);
        boxes[best] = a;
        boxes.push_back(b);
    }
    pal.n = int(boxes.size());
    for (size_t k = 0; k < boxes.size(); ++k) {
        uint64_t s[3] = {0, 0, 0}, c = 0;
        for (int i = boxes[k].begin; i < boxes[k].end; ++i) {
            for (int a = 0; a < 3; ++a) s[a] += sum[size_t(bins[i]) * 3 + a];
            c += count[bins[i]];
        }
        for (int a = 0; a < 3; ++a) pal.rgb[k * 3 + a] = uint8_t(c ? (s[a] + c / 2) / c : 0);
    }

    // Bin -> nearest palette colour (from the bin's mean; bins only reached by
    // dithering from their centre), computed once per bin.
    std::vector<int16_t> lut(kBins, -1);
    auto nearest = [&](int bin) -> int {
        int16_t& l = lut[bin];
        if (l >= 0) return l;
        int r, g, b;
        if (count[bin]) {
            r = int(sum[size_t(bin) * 3] / count[bin]);
            g = int(sum[size_t(bin) * 3 + 1] / count[bin]);
            b = int(sum[size_t(bin) * 3 + 2] / count[bin]);
        } else {
            r = chan(bin, 0) * 4 + 2;
            g = chan(bin, 1) * 4 + 2;
            b = chan(bin, 2) * 4 + 2;
        }
        int best = 0, bestD = 1 << 30;
        for (int k = 0; k < pal.n; ++k) {
            const int dr = r - pal.rgb[k * 3], dg = g - pal.rgb[k * 3 + 1], db = b - pal.rgb[k * 3 + 2];
            const int d = dr * dr + dg * dg + db * db;
            if (d < bestD) {
                bestD = d;
                best = k;
            }
        }
        l = int16_t(best);
        return best;
    };
    if (!dither) {
        for (size_t i = 0; i < n; ++i) {
            const unsigned char* p = rgba + i * 4;
            index[i] = uint8_t(nearest(binOf(p[0], p[1], p[2])));
        }
        return;
    }
    // Floyd-Steinberg: the error of each pixel spread to its neighbours.
    std::vector<float> err(size_t(w + 2) * 3 * 2, 0.f);
    for (int y = 0; y < h; ++y) {
        float* cur = &err[size_t(y & 1) * (w + 2) * 3];
        float* nxt = &err[size_t((y + 1) & 1) * (w + 2) * 3];
        std::fill(nxt, nxt + size_t(w + 2) * 3, 0.f);
        for (int x = 0; x < w; ++x) {
            const unsigned char* p = rgba + (size_t(y) * w + x) * 4;
            int c[3];
            for (int a = 0; a < 3; ++a) c[a] = std::clamp(int(std::lround(p[a] + cur[(x + 1) * 3 + a])), 0, 255);
            const int k = nearest(binOf(c[0], c[1], c[2]));
            index[size_t(y) * w + x] = uint8_t(k);
            for (int a = 0; a < 3; ++a) {
                const float e = float(c[a] - pal.rgb[k * 3 + a]);
                cur[(x + 2) * 3 + a] += e * 7 / 16;
                nxt[x * 3 + a] += e * 3 / 16;
                nxt[(x + 1) * 3 + a] += e * 5 / 16;
                nxt[(x + 2) * 3 + a] += e * 1 / 16;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

GifWriter::~GifWriter() = default;

bool GifWriter::put(const void* p, size_t n) {
    if (ok_ && !f_.write(static_cast<const char*>(p), std::streamsize(n))) ok_ = false;
    return ok_;
}

bool GifWriter::open(const std::string& path, int w, int h, int loops, std::string& error) {
    if (w <= 0 || h <= 0 || w > 65535 || h > 65535) {
        error = "a GIF is at most 65535 pixels per side";
        return false;
    }
    f_.open(fs::u8path(path), std::ios::binary | std::ios::trunc);
    if (!f_) {
        error = "could not create " + path;
        return false;
    }
    w_ = w;
    h_ = h;
    // Logical screen without a global colour table (each frame has its own).
    const uint8_t head[13] = {'G', 'I', 'F', '8', '9', 'a', uint8_t(w), uint8_t(w >> 8), uint8_t(h), uint8_t(h >> 8),
                              0x70, 0, 0};
    put(head, sizeof(head));
    const uint8_t loop[19] = {0x21, 0xFF, 11, 'N', 'E', 'T', 'S', 'C', 'A', 'P', 'E', '2', '.', '0',
                              3, 1, uint8_t(loops), uint8_t(loops >> 8), 0};
    put(loop, sizeof(loop));
    if (!ok_) error = "could not write " + path;
    return ok_;
}

bool GifWriter::addFrame(const unsigned char* rgba, int delayCs, bool dither, std::string& error) {
    auto t0 = std::chrono::steady_clock::now();
    GifPalette pal;
    gifQuantize(rgba, w_, h_, dither, pal, index_);
    lastQuantizeMs = msSince(t0);
    t0 = std::chrono::steady_clock::now();

    // Graphic control: the delay; disposal 1 (each frame covers the whole screen).
    const uint8_t gce[8] = {0x21, 0xF9, 4, 0x04, uint8_t(delayCs), uint8_t(delayCs >> 8), 0, 0};
    put(gce, sizeof(gce));
    // Image descriptor with a local colour table of 256 entries (unused ones black).
    const uint8_t desc[10] = {0x2C, 0, 0, 0, 0, uint8_t(w_), uint8_t(w_ >> 8), uint8_t(h_), uint8_t(h_ >> 8), 0x87};
    put(desc, sizeof(desc));
    uint8_t table[256 * 3] = {};
    std::memcpy(table, pal.rgb, size_t(pal.n) * 3);
    put(table, sizeof(table));

    // LZW, 8-bit minimum code size (codes 9 to 12 bits), the dictionary in an
    // open-addressing hash of (prefix code, byte) as in compress/giflib.
    constexpr int kMin = 8, kClear = 1 << kMin, kEoi = kClear + 1, kHash = 5003;
    std::vector<int32_t> hkey(kHash, -1);
    std::vector<int16_t> hcode(kHash, 0);
    out_.clear();
    uint32_t acc = 0;
    int bits = 0;
    auto emit = [&](int code, int size) {
        acc |= uint32_t(code) << bits;
        bits += size;
        while (bits >= 8) {
            out_.push_back(uint8_t(acc));
            acc >>= 8;
            bits -= 8;
        }
    };
    int codeSize = kMin + 1, maxCode = kEoi;
    emit(kClear, codeSize);
    const size_t n = index_.size();
    int cur = index_[0];
    for (size_t i = 1; i < n; ++i) {
        const int c = index_[i];
        const int32_t key = (cur << 8) | c;
        int h = int((uint32_t(c) << 12 ^ uint32_t(cur)) % kHash);
        while (hkey[h] != -1 && hkey[h] != key) h = h + 1 == kHash ? 0 : h + 1;
        if (hkey[h] == key) {
            cur = hcode[h];
            continue;
        }
        emit(cur, codeSize);
        hkey[h] = key;
        hcode[h] = int16_t(++maxCode);
        if (maxCode >= (1 << codeSize)) ++codeSize;
        if (maxCode == 4095) {
            emit(kClear, codeSize);
            std::fill(hkey.begin(), hkey.end(), -1);
            codeSize = kMin + 1;
            maxCode = kEoi;
        }
        cur = c;
    }
    emit(cur, codeSize);
    // A decoder adds one more entry on reading that last code (and widens its
    // codes one entry before the encoder does): the end code at its width.
    emit(kEoi, maxCode + 1 == (1 << codeSize) && codeSize < 12 ? codeSize + 1 : codeSize);
    if (bits > 0) out_.push_back(uint8_t(acc));

    const uint8_t minSize = kMin;
    put(&minSize, 1);
    for (size_t i = 0; i < out_.size(); i += 255) {
        const uint8_t len = uint8_t(std::min<size_t>(255, out_.size() - i));
        put(&len, 1);
        put(out_.data() + i, len);
    }
    const uint8_t end = 0;
    put(&end, 1);
    lastEncodeMs = msSince(t0);
    if (!ok_) error = "could not write the GIF (disk full?)";
    return ok_;
}

bool GifWriter::close(std::string& error) {
    if (!f_.is_open()) return false;
    const uint8_t trailer = 0x3B;
    put(&trailer, 1);
    f_.close();
    if (f_.fail()) ok_ = false;
    if (!ok_) error = "could not write the GIF (disk full?)";
    return ok_;
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

bool gifDecode(const std::string& path, GifImage& img, std::string& error) {
    std::ifstream f(fs::u8path(path), std::ios::binary);
    if (!f) {
        error = "could not open " + path;
        return false;
    }
    const std::vector<uint8_t> d((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    size_t p = 0;
    auto need = [&](size_t k) { return p + k <= d.size(); };
    auto u16 = [&](size_t at) { return int(d[at] | d[at + 1] << 8); };
    if (!need(13) || std::memcmp(d.data(), "GIF8", 4) != 0) {
        error = "not a GIF";
        return false;
    }
    img = GifImage();
    img.w = u16(6);
    img.h = u16(8);
    const uint8_t flags = d[10];
    p = 13;
    std::vector<uint8_t> global;
    if (flags & 0x80) {
        const size_t k = size_t(3) << ((flags & 7) + 1);
        if (!need(k)) return error = "truncated colour table", false;
        global.assign(d.begin() + p, d.begin() + p + k);
        p += k;
    }
    std::vector<unsigned char> canvas(size_t(img.w) * img.h * 4, 0);
    int delay = 0, transparent = -1;
    auto skipBlocks = [&]() {
        while (need(1) && d[p]) {
            p += 1 + d[p];
        }
        ++p;
    };
    while (need(1)) {
        const uint8_t tag = d[p++];
        if (tag == 0x3B) return true;
        if (tag == 0x21) {
            if (!need(1)) break;
            const uint8_t label = d[p++];
            if (label == 0xF9 && need(6)) {
                delay = u16(p + 2);
                transparent = (d[p + 1] & 1) ? d[p + 4] : -1;
                p += 1 + d[p];
                skipBlocks();
            } else if (label == 0xFF && need(12) && std::memcmp(&d[p + 1], "NETSCAPE2.0", 11) == 0) {
                p += 1 + d[p];
                if (need(4) && d[p] == 3 && d[p + 1] == 1) img.loops = u16(p + 2);
                skipBlocks();
            } else {
                skipBlocks();
            }
            continue;
        }
        if (tag != 0x2C || !need(9)) {
            error = "unexpected block in the GIF";
            return false;
        }
        const int fx = u16(p), fy = u16(p + 2), fw = u16(p + 4), fh = u16(p + 6);
        const uint8_t lf = d[p + 8];
        p += 9;
        std::vector<uint8_t> table = global;
        if (lf & 0x80) {
            const size_t k = size_t(3) << ((lf & 7) + 1);
            if (!need(k)) return error = "truncated colour table", false;
            table.assign(d.begin() + p, d.begin() + p + k);
            p += k;
        }
        if (lf & 0x40) {
            error = "interlaced frames are not decoded";
            return false;
        }
        if (!need(1)) break;
        const int minSize = d[p++];
        std::vector<uint8_t> data;
        while (need(1) && d[p]) {
            const size_t k = d[p];
            if (!need(1 + k)) return error = "truncated image data", false;
            data.insert(data.end(), d.begin() + p + 1, d.begin() + p + 1 + k);
            p += 1 + k;
        }
        ++p;
        // LZW decode.
        const int clear = 1 << minSize, eoi = clear + 1;
        std::vector<uint16_t> prefix(4096);
        std::vector<uint8_t> suffix(4096), stack(4097);
        for (int i = 0; i < clear; ++i) suffix[i] = uint8_t(i);
        std::vector<uint8_t> indices;
        indices.reserve(size_t(fw) * fh);
        int size = minSize + 1, next = eoi + 1, prev = -1;
        uint8_t first = 0;
        uint32_t acc = 0;
        int bits = 0;
        size_t q = 0;
        for (;;) {
            while (bits < size && q < data.size()) {
                acc |= uint32_t(data[q++]) << bits;
                bits += 8;
            }
            if (bits < size) break;
            const int code = int(acc & ((1u << size) - 1));
            acc >>= size;
            bits -= size;
            if (code == clear) {
                size = minSize + 1;
                next = eoi + 1;
                prev = -1;
                continue;
            }
            if (code == eoi) break;
            int c = code, sp = 0;
            if (prev < 0) {
                if (code >= clear) return error = "bad LZW stream", false;
                indices.push_back(uint8_t(code));
                first = uint8_t(code);
                prev = code;
                continue;
            }
            if (code > next || code >= 4096) return error = "bad LZW code", false;
            if (code == next) {
                stack[sp++] = first;
                c = prev;
            }
            while (c >= clear) {
                stack[sp++] = suffix[c];
                c = prefix[c];
            }
            stack[sp++] = uint8_t(c);
            first = uint8_t(c);
            while (sp) indices.push_back(stack[--sp]);
            if (next < 4096) {
                prefix[next] = uint16_t(prev);
                suffix[next] = first;
                ++next;
                if (next == (1 << size) && size < 12) ++size;
            }
            prev = code;
        }
        if (indices.size() < size_t(fw) * fh) return error = "short image data", false;
        for (int y = 0; y < fh; ++y)
            for (int x = 0; x < fw; ++x) {
                const int k = indices[size_t(y) * fw + x];
                const int cx = fx + x, cy = fy + y;
                if (k == transparent || cx >= img.w || cy >= img.h || size_t(k) * 3 + 2 >= table.size()) continue;
                unsigned char* o = &canvas[(size_t(cy) * img.w + cx) * 4];
                o[0] = table[k * 3];
                o[1] = table[k * 3 + 1];
                o[2] = table[k * 3 + 2];
                o[3] = 255;
            }
        img.frames.push_back({delay, canvas});
        delay = 0;
        transparent = -1;
    }
    error = "the GIF has no trailer";
    return false;
}

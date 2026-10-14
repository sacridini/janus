#pragma once

// Animated GIF, written without an image library: each frame gets its own
// palette of up to 256 colours (exact when the frame has no more, else median
// cut), LZW-compressed, looping forever (NETSCAPE2.0). A small decoder reads
// them back (the self-test).

#include <cstdint>
#include <fstream>
#include <string>
#include <vector>

struct GifPalette {
    int n = 0;                    // colours used (1..256)
    uint8_t rgb[256 * 3] = {};
};

// The frame's colours (RGBA8 rows, alpha ignored) as palette indices: an exact
// palette when there are 256 colours or fewer, else a median cut of a 6-bit
// histogram; `dither` diffuses the error (Floyd-Steinberg).
void gifQuantize(const unsigned char* rgba, int w, int h, bool dither, GifPalette& pal, std::vector<uint8_t>& index);

class GifWriter {
public:
    GifWriter() = default;
    ~GifWriter();
    GifWriter(const GifWriter&) = delete;
    GifWriter& operator=(const GifWriter&) = delete;
    // Header, logical screen and the loop extension (loops: 0 = forever).
    bool open(const std::string& path, int w, int h, int loops, std::string& error);
    // A full frame (w x h RGBA8, rows top-down) shown for `delayCs` hundredths of a second.
    bool addFrame(const unsigned char* rgba, int delayCs, bool dither, std::string& error);
    bool close(std::string& error); // trailer; false if anything could not be written
    double lastQuantizeMs = 0, lastEncodeMs = 0;

private:
    bool put(const void* p, size_t n);
    std::ofstream f_;
    int w_ = 0, h_ = 0;
    bool ok_ = true;
    std::vector<uint8_t> index_, out_;
};

// A decoded GIF: every frame composed onto the canvas (RGBA8, w x h).
struct GifImage {
    int w = 0, h = 0;
    int loops = -1;               // NETSCAPE2.0 loop count (0 = forever), -1 without the extension
    struct Frame {
        int delayCs = 0;
        std::vector<unsigned char> rgba;
    };
    std::vector<Frame> frames;
};
bool gifDecode(const std::string& path, GifImage& img, std::string& error);

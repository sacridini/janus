// Animation export: the map over a range of dates (or bands of a spectrum) as
// a GIF, MP4 or WebM. Frames are rendered on the main thread like the PNG
// export (offscreen, with the date stamp, legend and scale bar drawn on the
// CPU), one per UI frame once the date, its detail tiles and the basemap are on
// the GPU; the view stays frozen behind a modal progress popup. A background
// thread encodes them from a short queue (a few frames in memory, whatever the
// length): our own GIF writer (gif.hpp), or ffmpeg fed raw RGBA on its
// standard input (no library linked; found in Settings, next to Janus or on PATH).
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <deque>
#include <filesystem>
#include <fstream>
#include <mutex>
#include <sstream>

#include <imgui_internal.h>

#include "gif.hpp"
#include "glfw.hpp"
#include "platform.hpp"
#include "profiler.hpp"

namespace fs = std::filesystem;

namespace {

constexpr double kTileWaitS = 20;     // longest wait for a frame's detail tiles / basemap tiles

double secondsSince(std::chrono::steady_clock::time_point t) {
    return std::chrono::duration<double>(std::chrono::steady_clock::now() - t).count();
}

bool timeDependent(int m) { return m == ModeValue || m == ModeAnomaly || m == ModeRGB || m == ModeDiff || m == ModeComposite; }
bool needsStats(int m) { return m != ModeValue && m != ModeRGB && m != ModeDiff && m != ModeComposite; }

std::string escapePercent(const std::string& s) {
    std::string out;
    for (char c : s) {
        out += c;
        if (c == '%') out += '%';
    }
    return out;
}

// Frames from the main thread to the encoder: at most kMax wait in memory.
struct FrameQueue {
    static constexpr size_t kMax = 3;
    std::mutex m;
    std::condition_variable cv;
    std::deque<std::vector<unsigned char>> frames;
    bool closed = false;
    std::string abort;           // set by the main thread: the export fails with it
    bool full() {
        std::lock_guard<std::mutex> lk(m);
        return frames.size() >= kMax;
    }
    void push(std::vector<unsigned char>&& f) {
        {
            std::lock_guard<std::mutex> lk(m);
            frames.push_back(std::move(f));
        }
        cv.notify_all();
    }
    void close(const std::string& why = "") {
        {
            std::lock_guard<std::mutex> lk(m);
            closed = true;
            if (!why.empty()) abort = why;
        }
        cv.notify_all();
    }
    // The next frame; false at the end (closed and empty), on cancel or abort.
    bool pop(std::vector<unsigned char>& f, const ExportJob& j, std::string& error) {
        std::unique_lock<std::mutex> lk(m);
        while (frames.empty() && !closed && !j.cancel) cv.wait_for(lk, std::chrono::milliseconds(50));
        if (!abort.empty()) {
            error = abort;
            return false;
        }
        if (j.cancel || frames.empty()) return false;
        f = std::move(frames.front());
        frames.pop_front();
        return true;
    }
};

// Measured by the encoder, for the self-test's report.
struct AnimStats {
    std::atomic<int> frames{0};
    std::atomic<double> quantizeMs{0}, encodeMs{0}; // GIF: sums over the frames
};
AnimStats gStats;

// The last lines of a log (ffmpeg's errors).
std::string logTail(const std::string& path, int lines) {
    std::ifstream f(fs::u8path(path), std::ios::binary);
    std::string text((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    while (!text.empty() && (text.back() == '\n' || text.back() == '\r')) text.pop_back();
    size_t at = text.size();
    for (int k = 0; k < lines && at != std::string::npos && at > 0; ++k) at = text.rfind('\n', at - 1);
    std::string out = at == std::string::npos ? text : text.substr(at + 1);
    out.erase(std::remove(out.begin(), out.end(), '\r'), out.end());
    return out;
}

} // namespace

struct App::AnimRun {
    AnimOptions o;
    std::vector<int> dates;
    size_t next = 0;             // the frame being prepared
    int origT = 0;
    uint64_t cube = 0;
    int w = 0, h = 0;            // size of every frame
    std::shared_ptr<FrameQueue> queue;
    std::shared_ptr<ExportJob> job;
    std::chrono::steady_clock::time_point waitSince{};
    bool waiting = false;        // waitSince is set for `next`
    std::string status;          // what the frame waits for
    int lateTiles = 0;           // frames drawn without all their detail tiles (timeout)
};

const char* App::animExt(int format) { return format == 1 ? "mp4" : format == 2 ? "webm" : "gif"; }

std::string App::findFfmpeg() const {
    // Asked of the file system at most every 2 s (the export popup asks every frame).
    static std::string cachedFor = "\x01", cached;
    static std::chrono::steady_clock::time_point at{};
    if (cachedFor == ffmpegPath_ && secondsSince(at) < 2) return cached;
    std::error_code ec;
    std::string found;
#ifdef _WIN32
    const char* exe = "ffmpeg.exe";
#else
    const char* exe = "ffmpeg";
#endif
    if (!ffmpegPath_.empty()) {
        if (fs::is_regular_file(fs::u8path(ffmpegPath_), ec)) found = ffmpegPath_;
    } else if (fs::is_regular_file(fs::u8path(platform::exeDir()) / exe, ec)) {
        found = (fs::u8path(platform::exeDir()) / exe).u8string();
    } else {
        found = platform::findOnPath("ffmpeg");
    }
    cachedFor = ffmpegPath_;
    cached = found;
    at = std::chrono::steady_clock::now();
    return found;
}

std::vector<int> App::animDates(const AnimOptions& o) const {
    std::vector<int> d;
    if (!s_) return d;
    const int T = s_->info->T();
    const int from = std::clamp(o.from, 0, T - 1), to = std::clamp(o.to < 0 ? T - 1 : o.to, 0, T - 1);
    const int step = std::max(1, o.step);
    if (from <= to)
        for (int t = from; t <= to; t += step) d.push_back(t);
    else // backwards in time
        for (int t = from; t >= to; t -= step) d.push_back(t);
    return d;
}

// ---------------------------------------------------------------------------
// Options (the export popup, exportKind_ 5)
// ---------------------------------------------------------------------------

void App::uiAnimationOptions(bool& canSave) {
    PROFILE_ZONE();
    const CubeInfo& info = *s_->info;
    const int T = info.T();
    const char* unit = info.spectral ? "band" : "date";
    anim_.from = std::clamp(anim_.from, 0, T - 1);
    if (anim_.to < 0 || anim_.to >= T) anim_.to = T - 1;
    ImGui::TextColored(theme::accent(), "%s", activeLayer() ? activeLayer()->name.c_str() : "");
    const float w = ImGui::GetFontSize() * 20;
    ImGui::SetNextItemWidth(w);
    ImGui::SliderInt("From", &anim_.from, 0, T - 1, escapePercent(info.layers[anim_.from].label).c_str(),
                     ImGuiSliderFlags_AlwaysClamp);
    ImGui::SetNextItemWidth(w);
    ImGui::SliderInt("To", &anim_.to, 0, T - 1, escapePercent(info.layers[anim_.to].label).c_str(),
                     ImGuiSliderFlags_AlwaysClamp);
    ImGui::SetItemTooltip("A range that ends before it starts plays backwards.");
    ImGui::SetNextItemWidth(w);
    char stepFmt[48];
    std::snprintf(stepFmt, sizeof(stepFmt), "every %%d %s(s)", unit);
    ImGui::SliderInt("Step", &anim_.step, 1, std::max(1, T - 1), stepFmt, ImGuiSliderFlags_AlwaysClamp);
    ImGui::SetNextItemWidth(w);
    ImGui::SliderFloat("Frame rate", &anim_.fps, 0.5f, 30.f, "%.1f frames/s", ImGuiSliderFlags_Logarithmic);
    const int n = int(animDates(anim_).size());
    ImGui::TextDisabled("%d frame%s, %.1f s", n, n == 1 ? "" : "s", n / std::max(0.1f, anim_.fps));
    if (anim_.format == 0) {
        const int cs = std::max(2, int(std::lround(100.0 / anim_.fps)));
        if (std::fabs(cs / 100.0 - 1.0 / anim_.fps) > 0.004)
            ImGui::TextDisabled("GIF: each frame lasts %.2f s (in hundredths of a second)", cs / 100.0);
    }

    ImGui::Spacing();
    ImGui::TextUnformatted("Resolution (rendered at it, not enlarged):");
    for (int s : {1, 2}) {
        char label[64];
        std::snprintf(label, sizeof(label), "%dx  (%ld x %ld px)", s, std::lround(int(canvasSize_.x) * mapPixelScale_ * s),
                      std::lround(int(canvasSize_.y) * mapPixelScale_ * s));
        const bool fits = exportScaleFits(s);
        if (!fits && anim_.scale == s) anim_.scale = 1;
        ImGui::BeginDisabled(!fits);
        ImGui::RadioButton(label, &anim_.scale, s);
        ImGui::EndDisabled();
        if (!fits) ImGui::SetItemTooltip("Too large for the GPU");
    }

    ImGui::Spacing();
    const std::string ffmpeg = findFfmpeg();
    static const char* kFormats[] = {"GIF (any browser, slides; 256 colours a frame)", "MP4 (H.264, ffmpeg)",
                                     "WebM (VP9, ffmpeg)"};
    const int oldFormat = anim_.format;
    ImGui::SetNextItemWidth(w);
    if (ImGui::BeginCombo("Format", kFormats[anim_.format])) {
        for (int f = 0; f < 3; ++f) {
            ImGui::BeginDisabled(f > 0 && ffmpeg.empty());
            if (ImGui::Selectable(kFormats[f], anim_.format == f)) anim_.format = f;
            ImGui::EndDisabled();
            if (f > 0 && ffmpeg.empty())
                ImGui::SetItemTooltip("Needs ffmpeg: install it (e.g. winget install ffmpeg, apt install ffmpeg,\n"
                                      "brew install ffmpeg) or set its path in File > Settings.");
        }
        ImGui::EndCombo();
    }
    if (anim_.format > 0 && ffmpeg.empty()) anim_.format = 0;
    if (anim_.format != oldFormat) { // the file name follows the format
        std::string p = exportPath_;
        const size_t dot = p.find_last_of('.'), sep = p.find_last_of("/\\");
        if (dot != std::string::npos && (sep == std::string::npos || dot > sep)) {
            std::string e = p.substr(dot + 1);
            for (char& c : e) c = char(std::tolower((unsigned char)c));
            if (e == "gif" || e == "mp4" || e == "webm") p.erase(dot);
        }
        p += std::string(".") + animExt(anim_.format);
        std::snprintf(exportPath_, sizeof(exportPath_), "%s", p.c_str());
    }
    if (anim_.format > 0)
        ImGui::TextDisabled("ffmpeg: %s", ffmpeg.c_str());
    else if (ffmpeg.empty())
        ImGui::TextDisabled("MP4 and WebM need ffmpeg (not found; see File > Settings).");
    ImGui::SetNextItemWidth(w);
    ImGui::Combo("Background", &anim_.background, "Map's (dark)\0White\0");
    ImGui::Checkbox("Date stamp", &anim_.stamp);
    ImGui::SetItemTooltip("The %s large, top left, with the layer and the display mode under it", unit);
    ImGui::Checkbox("Legend (colour bar or classes)", &anim_.legend);
    ImGui::BeginDisabled(!info.hasGeoTransform || info.crsWkt.empty());
    ImGui::Checkbox("Scale bar", &anim_.scaleBar);
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("Ground distance at the centre of the view (the layer needs a CRS)");
    ImGui::Checkbox("Pins, ROI and vector layers", &anim_.marks);
    if (anim_.format == 0) {
        ImGui::Checkbox("Dither (GIF)", &anim_.dither);
        ImGui::SetItemTooltip("Spreads the error of the 256-colour palette: smoother gradients\n"
                              "(e.g. the basemap's imagery), larger files.");
    }
    if (!timeDependent(mode_) || embeddingView(*activeLayer()))
        ImGui::TextColored(theme::warning(), "The map shows %s, which does not change with the %s:\n"
                                             "every frame will be alike.", modeName(mode_, info), unit);
    else if (mode_ == ModeRGB)
        ImGui::TextColored(theme::warning(), "RGB composite: its three %ss are fixed, the frames change\n"
                                             "only where other layers follow the %s.", unit, unit);
    ImGui::TextDisabled("Drawn as the map shows it, waiting for each %s's detail tiles.", unit);
    if (n < 1) canSave = false;
    if (anim_.format > 0 && ffmpeg.empty()) canSave = false;
}

// ---------------------------------------------------------------------------
// The export
// ---------------------------------------------------------------------------

std::shared_ptr<ExportJob> App::startAnimation(const std::string& path, const AnimOptions& o) {
    if (!s_ || animRun_) return nullptr;
    auto run = std::make_shared<AnimRun>();
    run->o = o;
    run->dates = animDates(o);
    if (run->dates.empty()) return nullptr;
    run->origT = t_;
    run->cube = s_->info->id;
    const float ps = mapPixelScale_ * float(o.scale);
    run->w = int(std::lround(int(canvasSize_.x) * ps)); // as renderMap sizes its target
    run->h = int(std::lround(int(canvasSize_.y) * ps));
    run->queue = std::make_shared<FrameQueue>();
    playing_ = false;

    auto job = std::make_shared<ExportJob>();
    const char* kNames[] = {"GIF", "MP4", "WebM"};
    char title[200];
    std::snprintf(title, sizeof(title), "Animation as %s, %d frames of %d x %d px", kNames[std::clamp(o.format, 0, 2)],
                  int(run->dates.size()), run->w, run->h);
    job->title = title;
    job->path = path;
    const int total = int(run->dates.size()), w = run->w, h = run->h;
    const std::string ffmpeg = o.format > 0 ? findFfmpeg() : "";
    const std::string logPath = (fs::u8path(platform::appDataDir()) / "ffmpeg.log").u8string();
    const std::string threads = std::to_string(settings_.processingThreads());
    auto queue = run->queue;
    ExportJob* j = job.get(); // the job outlives the thread: its last member waits for it
    gStats.frames = 0;
    gStats.quantizeMs = 0;
    gStats.encodeMs = 0;
    job->done = std::async(std::launch::async, [j, queue, o, path, total, w, h, ffmpeg, logPath, threads] {
        const std::string tmp = path + ".part";
        std::string error;
        bool ok = true;
        int done = 0;
        std::vector<unsigned char> frame;
        if (o.format == 0) {
            GifWriter gif;
            const int delay = std::max(2, int(std::lround(100.0 / o.fps)));
            ok = gif.open(tmp, w, h, 0, error);
            while (ok && queue->pop(frame, *j, error)) {
                ok = gif.addFrame(frame.data(), delay, o.dither, error);
                gStats.quantizeMs = gStats.quantizeMs + gif.lastQuantizeMs;
                gStats.encodeMs = gStats.encodeMs + gif.lastEncodeMs;
                gStats.frames = ++done;
                j->progress = double(done) / total;
                glfwPostEmptyEvent();
            }
            std::string closeError;
            if (!gif.close(closeError) && ok) {
                ok = false;
                error = closeError;
            }
        } else {
            // Even sizes (yuv420p): the last column / row is left out when odd.
            const int vw = w & ~1, vh = h & ~1;
            char size[32], fps[32];
            std::snprintf(size, sizeof(size), "%dx%d", vw, vh);
            std::snprintf(fps, sizeof(fps), "%.6g", o.fps);
            std::vector<std::string> argv = {ffmpeg, "-hide_banner", "-loglevel", "warning", "-y", "-f", "rawvideo",
                                             "-pix_fmt", "rgba", "-s", size, "-framerate", fps, "-i", "-"};
            if (o.format == 1)
                argv.insert(argv.end(), {"-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p",
                                         "-movflags", "+faststart", "-threads", threads, "-f", "mp4", tmp});
            else
                argv.insert(argv.end(), {"-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0", "-deadline", "good",
                                         "-cpu-used", "4", "-row-mt", "1", "-pix_fmt", "yuv420p", "-threads", threads,
                                         "-f", "webm", tmp});
            platform::ChildProcess p;
            ok = p.start(argv, logPath, error);
            while (ok && queue->pop(frame, *j, error)) {
                for (int y = 0; y < vh && ok; ++y)
                    ok = p.write(frame.data() + size_t(y) * w * 4, size_t(vw) * 4);
                if (!ok) error = "ffmpeg stopped reading the frames";
                gStats.frames = ++done;
                j->progress = double(done) / total;
                glfwPostEmptyEvent();
            }
            if (j->cancel || !error.empty()) p.kill();
            const int code = p.finish();
            if (ok && error.empty() && !j->cancel && code != 0) {
                ok = false;
                error = "ffmpeg failed (exit code " + std::to_string(code) + ")";
            }
            if ((!ok || !error.empty()) && !j->cancel) {
                const std::string tail = logTail(logPath, 6);
                if (!tail.empty()) error += ":\n" + tail;
            }
        }
        if (!error.empty()) ok = false;
        if (ok && done < total && !j->cancel) {
            ok = false;
            error = "the export ended after " + std::to_string(done) + " of " + std::to_string(total) + " frames";
        }
        std::error_code ec;
        if (ok && !j->cancel) {
            fs::remove(fs::u8path(path), ec);
            fs::rename(fs::u8path(tmp), fs::u8path(path), ec);
            if (ec) {
                ok = false;
                error = "could not rename " + tmp + ": " + ec.message();
            }
        }
        if (!ok || j->cancel) fs::remove(fs::u8path(tmp), ec); // never a half-written file
        j->seconds = secondsSince(j->started);
        if (!ok) {
            std::lock_guard<std::mutex> lk(j->m);
            j->error = j->cancel ? "" : error;
        }
        j->state = j->cancel ? ExportJob::State::Cancelled : ok ? ExportJob::State::Done : ExportJob::State::Failed;
        glfwPostEmptyEvent();
    });
    run->job = job;
    animRun_ = run;
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

void App::pumpAnimation() {
    PROFILE_ZONE();
    if (!animRun_) return;
    AnimRun& r = *animRun_;
    // Rendering ends: the encoder finishes in the background (Exports window).
    auto end = [&](const std::string& why) {
        r.queue->close(why);
        if (s_ && s_->info->id == r.cube) setT(r.origT);
        animRun_.reset();
        mapDirty_ = true;
    };
    if (r.job->cancel || r.job->state != ExportJob::State::Running) return end("");
    if (!s_ || s_->info->id != r.cube) return end("the active layer changed during the export");
    if (r.next >= r.dates.size()) return end("");
    const int t = r.dates[r.next];
    if (t_ != t || !r.waiting) {
        setT(t); // the map asks for this date's tiles in this frame; the frame is drawn in a later one
        r.waiting = true;
        r.waitSince = std::chrono::steady_clock::now();
        r.status = "Loading...";
        return;
    }
    if (r.queue->full()) {
        r.status = "Encoding...";
        return;
    }
    const CubeInfo& info = *s_->info;
    // What the frame needs on the GPU: each visible layer's date(s) and statistics...
    for (const SeriesLayer& L : layers_) {
        if (!L.visible || !L.aligned || embeddingView(L)) continue;
        const bool act = &L == activeLayer();
        const int mode = act ? mode_ : L.disp.mode, lt = act ? t_ : L.disp.t;
        const std::array<int, 3>& rgb = act ? rgb_ : L.disp.rgb;
        const auto& loaded = L.session->gpu.loaded;
        const int LT = L.session->info->T();
        bool ready = true;
        if (needsStats(mode) && !L.session->gpu.statsValid) ready = false;
        else if (mode == ModeRGB) ready = loaded[rgb[0]] && loaded[rgb[1]] && loaded[rgb[2]];
        else if (timeDependent(mode)) ready = loaded[lt];
        if (ready && mode == ModeDiff) {
            const int ref = diffRefDate(act ? diffRef_ : L.disp.diffRef, lt, LT);
            ready = ref < 0 || loaded[ref];
        }
        if (ready && mode == ModeComposite) {
            DrawParams p;
            ready = temporalParams(*L.session, lt, p);
        }
        if (compositeOn(L)) ready = compositeReady(L, lt); // three bands as RGB: their image of the date
        if (!ready) {
            r.status = "Loading " + L.name + " at " + L.session->info->layers[std::clamp(lt, 0, LT - 1)].label + "...";
            return;
        }
    }
    // ...the active layer's detail tiles at this zoom, and the basemap's (up to kTileWaitS).
    const bool timedOut = secondsSince(r.waitSince) > kTileWaitS;
    if (detailLevel_ >= 0 && (mode_ == ModeValue || mode_ == ModeDiff) && !s_->deferRandomReads()) {
        const ViewRect v{-offset_.x / scale_, -offset_.y / scale_, (canvasSize_.x - offset_.x) / scale_,
                         (canvasSize_.y - offset_.y) / scale_, scale_ * mapPixelScale_};
        int missing = s_->tiles->missing(t_, v);
        if (mode_ == ModeDiff) {
            const int ref = diffRefDate(diffRef_, t_, info.T());
            if (ref >= 0) missing += s_->tiles->missing(ref, v);
        }
        if (r.next + 1 < r.dates.size()) s_->tiles->update(t_, v, r.dates[r.next + 1]); // read the next date meanwhile
        if (missing > 0 && !timedOut) {
            r.status = "Reading the detail tiles (" + std::to_string(missing) + " left)...";
            return;
        }
        if (missing > 0) ++r.lateTiles;
    }
    if (basemapShown() && basemap_->inflight() > 0 && !basemap_->offline() && !timedOut) {
        r.status = "Loading the basemap...";
        return;
    }
    static const float kWhite[4] = {1, 1, 1, 1};
    std::vector<unsigned char> rgba;
    int w = 0, h = 0;
    renderExport(r.o.scale, false, r.o.background == 1 ? kWhite : nullptr, rgba, w, h);
    if (w != r.w || h != r.h) return end("the map was resized during the export");
    FigureOverlay fo;
    fo.label = r.o.stamp ? 2 : 0;
    fo.legend = r.o.legend;
    fo.marks = r.o.marks;
    fo.scaleBar = r.o.scaleBar;
    drawFigureOverlay(rgba, w, h, r.o.scale, fo);
    r.queue->push(std::move(rgba));
    ++r.next;
    r.waiting = false;
    if (r.next >= r.dates.size()) end("");
}

void App::uiAnimation() {
    PROFILE_ZONE();
    if (animRun_ && !ImGui::IsPopupOpen("Exporting animation###animrun")) ImGui::OpenPopup("Exporting animation###animrun");
    if (!ImGui::BeginPopupModal("Exporting animation###animrun", nullptr, ImGuiWindowFlags_AlwaysAutoResize)) return;
    if (!animRun_) {
        ImGui::CloseCurrentPopup();
        ImGui::EndPopup();
        return;
    }
    const AnimRun& r = *animRun_;
    const CubeInfo& info = *s_->info;
    const size_t k = std::min(r.next, r.dates.size() - 1);
    ImGui::Text("Frame %d of %d: %s", int(k) + 1, int(r.dates.size()), info.layers[r.dates[k]].label.c_str());
    ImGui::TextDisabled("%s", r.status.c_str());
    char overlay[64];
    std::snprintf(overlay, sizeof(overlay), "%d / %d encoded", gStats.frames.load(), int(r.dates.size()));
    ImGui::ProgressBar(float(r.next) / float(r.dates.size()), ImVec2(ImGui::GetFontSize() * 24, 0), overlay);
    ImGui::TextDisabled("The map stays as it is until the last frame is drawn.");
    if (ImGui::Button("Cancel", ImVec2(120, 0)) || ImGui::IsKeyPressed(ImGuiKey_Escape, false)) r.job->cancel = true;
    ImGui::EndPopup();
}

// ---------------------------------------------------------------------------
// Settings: the ffmpeg used for MP4 and WebM
// ---------------------------------------------------------------------------

void App::uiFfmpegSettings() {
    PROFILE_ZONE();
    ImGui::SeparatorText("Video export (ffmpeg)");
    static char buf[1024] = "";
    static std::string shown = "\x01";
    if (shown != ffmpegPath_) { // set elsewhere: the layout file, Browse...
        std::snprintf(buf, sizeof(buf), "%s", ffmpegPath_.c_str());
        shown = ffmpegPath_;
    }
    ImGui::SetNextItemWidth(-90);
    if (ImGui::InputTextWithHint("##ffmpeg", "next to Janus, else on PATH", buf, sizeof(buf))) {
        std::string p = buf;
        const size_t a = p.find_first_not_of(" \t\""), b = p.find_last_not_of(" \t\""); // as pasted from Explorer
        ffmpegPath_ = shown = a == std::string::npos ? "" : p.substr(a, b - a + 1);
        ImGui::MarkIniSettingsDirty();
    }
    ImGui::SetItemTooltip("The ffmpeg executable that writes MP4 and WebM animations (File > Export\n"
                          "animation). Empty: ffmpeg next to Janus, else the one on PATH.");
    ImGui::SameLine();
    if (ImGui::Button("Browse...", ImVec2(-1, 0))) {
        const std::vector<std::string> f = platform::openFilesDialog();
        if (!f.empty()) {
            ffmpegPath_ = f[0];
            ImGui::MarkIniSettingsDirty();
        }
    }
    const std::string found = findFfmpeg();
    if (!found.empty()) ImGui::TextDisabled("Using %s", found.c_str());
    else if (!ffmpegPath_.empty()) ImGui::TextColored(theme::warning(), "No such file: MP4 and WebM are off.");
    else ImGui::TextDisabled("Not found: MP4 and WebM are off (GIF needs nothing).");
}

// ---------------------------------------------------------------------------
// --selftest-ui stages 130-139
// ---------------------------------------------------------------------------

namespace {
double now() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

// Mean and largest |a - b| over a rectangle of two RGBA images of width w.
void compare(const std::vector<unsigned char>& a, const std::vector<unsigned char>& b, int w, int x0, int y0, int x1,
             int y1, double& mean, int& maxd) {
    double s = 0;
    size_t n = 0;
    maxd = 0;
    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            for (int c = 0; c < 3; ++c) {
                const int d = std::abs(int(a[(size_t(y) * w + x) * 4 + c]) - int(b[(size_t(y) * w + x) * 4 + c]));
                s += d;
                maxd = std::max(maxd, d);
                ++n;
            }
    mean = n ? s / n : 0;
}

// Pixels of a rectangle brighter than 230 in every channel (white text, the scale bar).
int whites(const std::vector<unsigned char>& a, int w, int x0, int y0, int x1, int y1) {
    int n = 0;
    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x) {
            const unsigned char* p = &a[(size_t(y) * w + x) * 4];
            n += p[0] > 230 && p[1] > 230 && p[2] > 230;
        }
    return n;
}

// The GIF writer alone: an image of <= 256 colours comes back exact, one of
// many colours within the palette's error, dithered or not.
const char* selfTestGifCodec(const std::string& dir) {
    const int w = 173, h = 91; // odd sizes, LZW resets (> 4096 codes)
    std::vector<unsigned char> img(size_t(w) * h * 4), smooth(img.size());
    for (int y = 0; y < h; ++y)
        for (int x = 0; x < w; ++x) {
            unsigned char* p = &img[(size_t(y) * w + x) * 4];
            const int k = (x * 7 + y * 13 + (x * y) % 11) % 251; // 251 colours
            p[0] = (unsigned char)(k * 37);
            p[1] = (unsigned char)(k * 11 + 3);
            p[2] = (unsigned char)(255 - k);
            p[3] = 255;
            unsigned char* q = &smooth[(size_t(y) * w + x) * 4];
            q[0] = (unsigned char)(x * 255 / (w - 1));
            q[1] = (unsigned char)(y * 255 / (h - 1));
            q[2] = (unsigned char)((x + y) * 255 / (w + h - 2));
            q[3] = 255;
        }
    const std::string path = (fs::u8path(dir) / "codec.gif").u8string();
    std::string error;
    GifWriter g;
    if (!g.open(path, w, h, 0, error) || !g.addFrame(img.data(), 7, false, error) ||
        !g.addFrame(smooth.data(), 8, false, error) || !g.addFrame(smooth.data(), 9, true, error) || !g.close(error)) {
        std::printf("    %s\n", error.c_str());
        return "GIF writer failed";
    }
    GifImage d;
    if (!gifDecode(path, d, error)) {
        std::printf("    %s\n", error.c_str());
        return "GIF decoder failed on the writer's file";
    }
    if (d.w != w || d.h != h || d.frames.size() != 3 || d.loops != 0) return "GIF: wrong size, frame count or loop";
    if (d.frames[0].delayCs != 7 || d.frames[1].delayCs != 8 || d.frames[2].delayCs != 9) return "GIF: wrong delays";
    double mean;
    int maxd;
    compare(d.frames[0].rgba, img, w, 0, 0, w, h, mean, maxd);
    if (maxd != 0) return "GIF: an image of 251 colours should come back exact";
    compare(d.frames[1].rgba, smooth, w, 0, 0, w, h, mean, maxd);
    std::printf("    GIF codec: 251 colours exact; %d x %d gradient within %.2f mean, %d max", w, h, mean, maxd);
    if (mean > 4 || maxd > 24) return "GIF: the median cut palette is too far from the gradient";
    compare(d.frames[2].rgba, smooth, w, 0, 0, w, h, mean, maxd);
    std::printf(" (dithered %.2f mean)\n", mean);
    if (mean > 8) return "GIF: dithered frame too far from the image";
    return nullptr;
}
} // namespace

int App::selfTestAnimation(const std::string& a) {
    static struct {
        std::string dir, gif, mp4;
        std::shared_ptr<ExportJob> job;
        AnimOptions o{};
        std::vector<int> dates;
        int origT = 0;
    } gTest;
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    switch (st_.stage) {
    case 130: {
        std::error_code ec;
        gTest.dir = (fs::temp_directory_path(ec) / "janus_selftest_anim").u8string();
        fs::remove_all(fs::u8path(gTest.dir), ec);
        fs::create_directories(fs::u8path(gTest.dir), ec);
        if (const char* e = selfTestGifCodec(gTest.dir)) return fail(e);
        openInputs({a});
        next("GIF writer and reader; open A again");
        break;
    }
    case 131: // A alone, loaded, on the map as it is laid out
        if (opening_.valid() || layers_.size() != 1 || layers_[0].inputs != std::vector<std::string>{a} ||
            !layers_[0].session->overview.complete() || !s_->gpu.statsValid ||
            canvasSize_.x < 100)
            break;
        {
            mode_ = ModeValue;
            range_[ModeValue] = Range{1.0f, 5.0f, true, 0};
            fitView(canvasSize_);
            setT(3);
            gTest.origT = t_;
            gTest.o = AnimOptions{};
            gTest.o.from = 2;
            gTest.o.to = 14;
            gTest.o.step = 3; // 2, 5, 8, 11, 14
            gTest.o.fps = 3;
            gTest.dates = animDates(gTest.o);
            gTest.gif = (fs::u8path(gTest.dir) / "a.gif").u8string();
            const double m = groundMetresPerPixel(150, 100);
            std::printf("    ground: %.3f m per pixel of A (30 m in UTM 23S, near its central meridian)\n", m);
            if (std::fabs(m - 30) > 0.3) return fail("the scale bar's ground distance is wrong");
            gTest.job = startAnimation(gTest.gif, gTest.o);
            if (!gTest.job || gTest.dates.size() != 5) return fail("the animation did not start");
            next("A animated: 5 dates as a GIF");
        }
        break;
    case 132: {
        if (animRun_) {
            if (t_ == gTest.origT && animRun_->next > 0) return fail("the date should follow the frames");
            break;
        }
        const ExportJob::State st = gTest.job->state;
        if (st == ExportJob::State::Running) break;
        if (st != ExportJob::State::Done) {
            std::printf("    %s\n", gTest.job->error.c_str());
            return fail("the GIF export failed");
        }
        if (t_ != gTest.origT) return fail("the date should be restored after the export");
        GifImage d;
        std::string error;
        if (!gifDecode(gTest.gif, d, error)) {
            std::printf("    %s\n", error.c_str());
            return fail("the exported GIF cannot be decoded");
        }
        const int n = gStats.frames.load();
        std::error_code ec;
        std::printf("    %s: %d x %d, %d frames, %.0f KB; %.1f ms to quantize and %.1f ms to compress a frame\n",
                    gTest.gif.c_str(), d.w, d.h, int(d.frames.size()), fs::file_size(fs::u8path(gTest.gif), ec) / 1024.0,
                    gStats.quantizeMs.load() / std::max(1, n), gStats.encodeMs.load() / std::max(1, n));
        if (d.frames.size() != gTest.dates.size()) return fail("the GIF should have one frame per date");
        if (d.loops != 0) return fail("the GIF should loop forever");
        for (const auto& f : d.frames)
            if (f.delayCs != 33) return fail("a frame at 3 frames/s lasts 33 hundredths");
        // Each frame against the map rendered again at its date: the map in
        // the middle (overlays are at the edges), the stamp and the scale bar at
        // the corners only in the GIF.
        const int W = d.w, H = d.h;
        std::vector<std::vector<unsigned char>> plain;
        for (int t : gTest.dates) {
            t_ = t;
            std::vector<unsigned char> img;
            int w = 0, h = 0;
            renderExport(1, false, nullptr, img, w, h);
            if (w != W || h != H) return fail("the GIF's size differs from the map's");
            plain.push_back(std::move(img));
        }
        t_ = gTest.origT;
        mapDirty_ = true;
        for (size_t k = 0; k < d.frames.size(); ++k) {
            double mean, other = 1e9;
            int maxd;
            compare(d.frames[k].rgba, plain[k], W, W * 3 / 10, H * 3 / 10, W * 7 / 10, H * 7 / 10, mean, maxd);
            for (size_t q = 0; q < plain.size(); ++q) {
                double m2;
                int x2;
                if (q != k) compare(d.frames[k].rgba, plain[q], W, W * 3 / 10, H * 3 / 10, W * 7 / 10, H * 7 / 10, m2, x2);
                if (q != k) other = std::min(other, m2);
            }
            const int stampG = whites(d.frames[k].rgba, W, 0, 0, W / 3, H / 6), stampP = whites(plain[k], W, 0, 0, W / 3, H / 6);
            const int barG = whites(d.frames[k].rgba, W, W * 3 / 4, H * 5 / 6, W, H);
            const int barP = whites(plain[k], W, W * 3 / 4, H * 5 / 6, W, H);
            std::printf("    frame %d (%s): |GIF - map| %.2f mean, %d max (other dates >= %.2f); stamp %d, scale bar %d "
                        "white px\n", int(k) + 1, s_->info->layers[gTest.dates[k]].label.c_str(), mean, maxd, other,
                        stampG - stampP, barG - barP);
            if (mean > 1.5 || maxd > 24) return fail("a GIF frame differs from the map at its date");
            if (other <= mean) return fail("a GIF frame matches another date better than its own");
            if (stampG - stampP < 50) return fail("the date stamp is missing");
            if (barG - barP < 30) return fail("the scale bar is missing");
        }
        next("the GIF: one frame per date, delays, loop, pixels, stamp and scale bar");
        if (findFfmpeg().empty()) {
            std::printf("    ffmpeg not found: MP4 not tested\n");
            st_.stage = 135;
            break;
        }
        gTest.o.format = 1;
        gTest.o.fps = 2;
        gTest.o.scale = 1;
        gTest.mp4 = (fs::u8path(gTest.dir) / "a.mp4").u8string();
        gTest.job = startAnimation(gTest.mp4, gTest.o);
        if (!gTest.job) return fail("the MP4 export did not start");
        break;
    }
    case 133: {
        if (animRun_ || gTest.job->state == ExportJob::State::Running) break;
        if (gTest.job->state != ExportJob::State::Done) {
            std::printf("    %s\n", gTest.job->error.c_str());
            return fail("the MP4 export failed");
        }
        std::error_code ec;
        const auto bytes = fs::file_size(fs::u8path(gTest.mp4), ec);
        std::printf("    %s: %.0f KB (ffmpeg %s)\n", gTest.mp4.c_str(), bytes / 1024.0, findFfmpeg().c_str());
        if (ec || bytes < 1000) return fail("the MP4 is empty");
        // ffprobe next to ffmpeg, if any: the number of frames.
        fs::path probe = fs::u8path(findFfmpeg());
        probe.replace_filename(probe.filename().u8string().replace(0, 6, "ffprobe")); // ffmpeg.exe -> ffprobe.exe
        if (fs::exists(probe, ec)) {
            const std::string log = (fs::u8path(gTest.dir) / "ffprobe.txt").u8string();
            platform::ChildProcess p;
            std::string error;
            if (p.start({probe.u8string(), "-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries",
                         "stream=nb_read_frames,width,height", "-of", "csv=p=0", gTest.mp4}, log, error)) {
                p.finish();
                std::ifstream f(fs::u8path(log));
                std::string line;
                std::getline(f, line);
                std::printf("    ffprobe: width,height,frames = %s\n", line.c_str());
                int fw = 0, fh = 0, frames = 0;
                char c1 = 0, c2 = 0;
                std::istringstream fields(line);
                fields >> fw >> c1 >> fh >> c2 >> frames;
                if (!fields || frames != int(gTest.dates.size()))
                    return fail("the MP4 should have one frame per date");
                if (fw % 2 || fh % 2 || std::abs(fw - int(canvasSize_.x * mapPixelScale_)) > 1)
                    return fail("the MP4's size should be the map's, even");
            }
        }
        next("the MP4 through ffmpeg");
        st_.stage = 135;
        break;
    }
    case 135: {
        // A cancelled export leaves nothing behind and restores the date.
        gTest.o.format = 0;
        gTest.o.from = 0;
        gTest.o.to = -1;
        gTest.o.step = 1;
        const std::string path = (fs::u8path(gTest.dir) / "cancelled.gif").u8string();
        gTest.gif = path;
        gTest.job = startAnimation(path, gTest.o);
        if (!gTest.job) return fail("the export did not start");
        next("an export started, to be cancelled");
        break;
    }
    case 136:
        if (animRun_ && animRun_->next < 3) break;
        gTest.job->cancel = true;
        next("cancelled after 3 frames");
        break;
    case 137: {
        if (animRun_ || gTest.job->state == ExportJob::State::Running) break;
        std::error_code ec;
        if (gTest.job->state != ExportJob::State::Cancelled) return fail("the export should be cancelled");
        if (fs::exists(fs::u8path(gTest.gif), ec) || fs::exists(fs::u8path(gTest.gif + ".part"), ec))
            return fail("a cancelled export must leave no file");
        if (t_ != gTest.origT) return fail("the date should be restored after a cancel");
        exports_.clear();
        if (platform::getEnv("JANUS_KEEP_ANIM").empty()) fs::remove_all(fs::u8path(gTest.dir), ec);
        next("cancel: no file left, the date restored");
        st_.stage = 140; // RGB composites of three bands per date (app_composite.cpp), which print OK
        return -1;
    }
    }
    return -1;
}

// `tsv --selftest-zeit <inputs>`: exercises the whole Zeit path without a window
// (open series -> start bridge -> pixel fit -> VRT -> raster job -> load result)
// and prints timings. Used to verify builds and installers non-interactively.
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <thread>

#include "cube.hpp"
#include "platform.hpp"
#include "results.hpp"
#include "selftest.hpp"
#include "zeit_client.hpp"

namespace fs = std::filesystem;
using Clock = std::chrono::steady_clock;

static double msSince(Clock::time_point t0) {
    return std::chrono::duration<double, std::milli>(Clock::now() - t0).count();
}

int runZeitSelfTest(const std::vector<std::string>& inputs, int band, const std::string& python,
                    const std::string& bridge) {
    auto fail = [](const std::string& what) {
        std::printf("FAIL: %s\n", what.c_str());
        return 1;
    };
    const auto t0 = Clock::now();
    std::string err;
    auto info = openCube(inputs, band, err);
    if (!info) return fail("open: " + err);
    std::printf("series: %s, %d x %d, %d dates, %s .. %s (%.0f ms)\n", info->description.c_str(), info->width,
                info->height, info->T(), info->layers.front().label.c_str(), info->layers.back().label.c_str(),
                msSince(t0));

    ZeitConfig cfg;
    const fs::path rt = fs::u8path(platform::exeDir()) / "runtime";
    cfg.python = python.empty() ? (rt / "python" / "python.exe").u8string() : python;
    cfg.bridge = bridge.empty() ? (rt / "tsv_zeit_bridge.py").u8string() : bridge;
    cfg.bundled = python.empty();
    cfg.logPath = (fs::u8path(platform::appDataDir()) / "zeit.log").u8string();
    ZeitClient zeit;
    zeit.start(cfg);
    const auto tz = Clock::now();
    while (zeit.state() == ZeitClient::State::Starting && msSince(tz) < 120000)
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    if (zeit.state() != ZeitClient::State::Ready) return fail("zeit: " + zeit.error());
    std::printf("zeit: %s (Python %s) ready in %.0f ms, %d tool(s)\n", zeit.zeitVersion().c_str(),
                zeit.pythonVersion().c_str(), zeit.startupMs(), int(zeit.tools().size()));
    const ZeitTool* lt = zeit.tool("landtrendr");
    if (!lt) return fail("landtrendr is not in the manifest");

    // Pixel fit at the center of the image.
    std::vector<double> years(info->T());
    for (int t = 0; t < info->T(); ++t) years[t] = info->decimalYear(t);
    std::vector<float> series(info->T());
    {
        CubeReader reader(info);
        for (int t = 0; t < info->T(); ++t) reader.readPixel(t, info->width / 2, info->height / 2, series[t]);
    }
    json params = json::object();
    for (const ZeitParam& p : lt->params) params[p.id] = p.def;
    const auto tp = Clock::now();
    const uint64_t id = zeit.runPixel("landtrendr", params, years, series);
    PixelReply reply{};
    bool got = false;
    while (!got && msSince(tp) < 30000) {
        for (PixelReply& r : zeit.takeReplies())
            if (r.id == id) {
                reply = r;
                got = true;
            }
        if (!got) std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    if (!got || !reply.ok) return fail("pixel fit: " + reply.error);
    std::printf("pixel fit: %.1f ms,", msSince(tp));
    for (const json& r : reply.result.value("rows", json::array()))
        std::printf(" [%s: %s]", r[0].get<std::string>().c_str(), r[1].get<std::string>().c_str());
    std::printf("\n");

    // Raster job on a 512 x 512 window at the center.
    const fs::path work = fs::u8path(platform::appDataDir()) / "selftest";
    std::error_code ec;
    fs::remove_all(work, ec);
    fs::create_directories(work, ec);
    const std::string vrt = (work / "cube.vrt").u8string();
    if (!writeCubeVrt(*info, vrt, err)) return fail("vrt: " + err);
    const int x0 = std::max(0, info->width / 2 - 256), y0 = std::max(0, info->height / 2 - 256);
    const int x1 = std::min(info->width, x0 + 512), y1 = std::min(info->height, y0 + 512);
    const json spec = {{"tool", "landtrendr"},  {"params", params},          {"input", vrt},
                       {"years", years},        {"nodata", nullptr},         {"window", {x0, y0, x1, y1}},
                       {"output_dir", (work / "out").u8string()}};
    const auto tj = Clock::now();
    auto job = zeit.startJob(spec, (work / "job.json").u8string(), "selftest");
    while (job->state == ZeitJob::State::Running && msSince(tj) < 600000)
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
    if (job->state != ZeitJob::State::Done) {
        std::lock_guard<std::mutex> lk(job->m);
        return fail("job: " + job->error);
    }
    std::printf("raster job (%d x %d px x %d dates): %.0f ms\n", x1 - x0, y1 - y0, info->T(), msSince(tj));

    json result;
    {
        std::lock_guard<std::mutex> lk(job->m);
        result = job->result;
    }
    const json& outs = result["outputs"];
    if (outs.empty()) return fail("job returned no outputs");
    ResultLayer L;
    L.path = outs[0].value("path", "");
    L.unit = outs[0].value("unit", "");
    if (!loadResultRaster(L.path, 4096, L.data, L.tw, L.th, err)) return fail("load result: " + err);
    autoResultRange(L);
    size_t valid = 0;
    for (float v : L.data) valid += std::isfinite(v);
    std::printf("result '%s': %d x %d, %.1f%% with events, range %.6g .. %.6g\n",
                outs[0].value("name", "").c_str(), L.tw, L.th, 100.0 * valid / L.data.size(), L.lo, L.hi);
    std::printf("OK (%.1f s total)\n", msSince(t0) / 1000.0);
    return 0;
}

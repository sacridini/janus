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

int runZeitSelfTest(const std::vector<std::string>& inputs, const BandSelection& sel, const std::string& python,
                    const std::string& bridge) {
    auto fail = [](const std::string& what) {
        std::printf("FAIL: %s\n", what.c_str());
        return 1;
    };
    const auto t0 = Clock::now();
    std::string err;
    auto info = openCube(inputs, sel, err);
    if (!info) return fail("open: " + err);
    std::printf("series: %s, %d x %d, %d dates, %s .. %s (%.0f ms)\n", info->description.c_str(), info->width,
                info->height, info->T(), info->layers.front().label.c_str(), info->layers.back().label.c_str(),
                msSince(t0));

    ZeitConfig cfg;
    const fs::path rt = fs::u8path(platform::exeDir()) / "runtime";
    cfg.python = python.empty() ? bundledPython(rt.u8string()) : python;
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

    // Series at the center of the image (pixel runs) and the cube as a VRT (jobs).
    std::vector<double> years(info->T());
    for (int t = 0; t < info->T(); ++t) years[t] = info->decimalYear(t);
    // The pixel: centre of the image, or the first one with no gaps in the shown series near it.
    CubeReader reader(info);
    std::vector<float> series(info->T());
    const int px = info->width / 2, py = info->height / 2;
    for (int t = 0; t < info->T(); ++t) reader.readPixel(t, px, py, series[t]);
    const BandRoles roles = guessBandRoles(*info);
    if (info->bandsPerDate > 1) {
        std::printf("bands per date: %d (shown: %s), roles:", info->bandsPerDate, info->selectionText().c_str());
        for (const auto& [role, b] : roles) std::printf(" %s=%d", role.c_str(), b);
        std::printf("\n");
    }
    const json extras = zeitPixelExtras(*info, roles);
    const fs::path work = fs::u8path(platform::appDataDir()) / "selftest";
    std::error_code ec;
    fs::remove_all(work, ec);
    fs::create_directories(work, ec);
    // 256 x 256 window at the center: enough to exercise chunking, quick for slow models.
    const int x0 = std::max(0, info->width / 2 - 128), y0 = std::max(0, info->height / 2 - 128);
    const int x1 = std::min(info->width, x0 + 256), y1 = std::min(info->height, y0 + 256);

    int ran = 0;
    for (const ZeitTool& tool : zeit.tools()) {
        std::string why = toolApplicability(tool, *info);
        if (why.empty() && !tool.bands.empty() && !missingBandRoles(tool, roles).empty())
            why = "unmapped bands: " + missingBandRoles(tool, roles);
        if (!why.empty()) {
            std::printf("\n[%s] skipped: %s\n", tool.id.c_str(), why.c_str());
            continue;
        }
        std::printf("\n[%s]\n", tool.id.c_str());
        json params = json::object();
        for (const ZeitParam& p : tool.params) params[p.id] = p.def;
        // "patterns" parameters (e.g. TWDTW classes): three pixels of the series,
        // as a user would add pins.
        for (const ZeitParam& p : tool.params) {
            if (p.type != "patterns") continue;
            json pats = json::array();
            for (int k = 0; k < 3; ++k) {
                const int x = info->width * (2 * k + 1) / 6, y = info->height / 2;
                json vals = json::array(), days = json::array();
                for (int t = 0; t < info->T(); ++t) {
                    float f = NAN;
                    reader.readPixel(t, x, y, f);
                    vals.push_back(std::isfinite(f) ? json(double(f)) : json());
                    days.push_back(info->timeIsDate ? json(info->unixDay(t)) : json());
                }
                pats.push_back({{"name", "Class " + std::to_string(k + 1) + " (" + std::to_string(x) + ", " +
                                             std::to_string(y) + ")"},
                                {"from", "selftest"},
                                {"years", years},
                                {"days", info->timeIsDate ? days : json(nullptr)},
                                {"values", vals}});
            }
            params[p.id] = pats;
        }

        if (tool.pixel) {
            const auto tp = Clock::now();
            json extra = extras;
            if (!tool.bands.empty()) extra.update(zeitPixelBands(reader, *info, tool, roles, px, py));
            const uint64_t id = zeit.runPixel(tool.id, params, years, series, extra);
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
            if (!got || !reply.ok) return fail(tool.id + " pixel run: " + reply.error);
            std::printf("  pixel: %.1f ms, overlays:", msSince(tp));
            for (const json& o : reply.result.value("overlays", json::array()))
                std::printf(" %s(%s, %d)", o.value("type", "?").c_str(), o.value("label", "").c_str(),
                            int(o.value("x", json::array()).size()));
            std::printf("\n");
            for (const json& r : reply.result.value("rows", json::array()))
                std::printf("    %s: %s\n", r[0].get<std::string>().c_str(), r[1].get<std::string>().c_str());
        }

        if (tool.raster) {
            json spec = {{"tool", tool.id}, {"params", params}};
            if (!zeitJobInputs(*info, tool, roles, work.u8string(), "cube", spec, err)) return fail("vrt: " + err);
            spec["window"] = {x0, y0, x1, y1};
            // Run-time estimate (as shown in the tool window) before the real run.
            double estimate = -1;
            {
                const auto te = Clock::now();
                const uint64_t id = zeit.call("estimate", spec);
                bool got = false;
                while (!got && msSince(te) < 60000) {
                    for (PixelReply& r : zeit.takeReplies())
                        if (r.id == id) {
                            got = true;
                            if (!r.ok) return fail(tool.id + " estimate: " + r.error);
                            estimate = estimateJobSeconds(r.result, x1 - x0, y1 - y0);
                        }
                    if (!got) std::this_thread::sleep_for(std::chrono::milliseconds(1));
                }
                if (!got) return fail(tool.id + " estimate: no reply");
                std::printf("  estimate: %.2f s of computing (asked in %.0f ms)\n", estimate, msSince(te));
            }
            spec["output_dir"] = (work / tool.id).u8string();
            const auto tj = Clock::now();
            auto job = zeit.startJob(spec, (work / (tool.id + ".json")).u8string(), "selftest " + tool.id);
            while (job->state == ZeitJob::State::Running && msSince(tj) < 600000)
                std::this_thread::sleep_for(std::chrono::milliseconds(20));
            json result;
            {
                std::lock_guard<std::mutex> lk(job->m);
                if (job->state != ZeitJob::State::Done) return fail(tool.id + " job: " + job->error);
                result = job->result;
            }
            std::printf("  raster job (%d x %d px x %d dates): %.0f ms\n", x1 - x0, y1 - y0, info->T(), msSince(tj));
            const json& outs = result["outputs"];
            if (outs.size() != tool.outputs.size()) return fail(tool.id + ": job returned a different set of outputs");
            for (const json& o : outs) {
                ResultLayer L;
                L.path = o.value("path", "");
                L.unit = o.value("unit", "");
                if (!loadResultRaster(L.path, 4096, L.data, L.tw, L.th, err)) return fail("load result: " + err);
                autoResultRange(L);
                size_t valid = 0;
                for (float v : L.data) valid += std::isfinite(v);
                std::printf("    %-28s %5.1f%% valid, range %.6g .. %.6g\n", o.value("name", "").c_str(),
                            100.0 * valid / L.data.size(), L.lo, L.hi);
                const auto classes = o.value("classes", std::vector<std::string>());
                if (!classes.empty()) {
                    std::vector<size_t> count(classes.size() + 1, 0);
                    for (float f : L.data)
                        if (std::isfinite(f) && f >= 1 && f <= classes.size()) ++count[size_t(std::lround(f))];
                    for (size_t k = 0; k < classes.size(); ++k)
                        std::printf("      %-26s %5.1f%%\n", classes[k].c_str(), 100.0 * count[k + 1] / L.data.size());
                }
            }
        }
        ++ran;
    }
    if (ran == 0) return fail("no tool applies to this series");

    std::printf("OK (%.1f s total)\n", msSince(t0) / 1000.0);
    return 0;
}

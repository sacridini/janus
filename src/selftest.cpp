// `jn --selftest-zeit <inputs>`: exercises the whole Zeit path without a window
// (open series -> start bridge -> pixel fit -> VRT -> raster job -> load result)
// and prints timings. Used to verify builds and installers non-interactively.
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <thread>

#include "cube.hpp"
#include "multidim.hpp"
#include "platform.hpp"
#include "results.hpp"
#include "selftest.hpp"
#include "zeit_client.hpp"

namespace fs = std::filesystem;
using Clock = std::chrono::steady_clock;

static double msSince(Clock::time_point t0) {
    return std::chrono::duration<double, std::milli>(Clock::now() - t0).count();
}

int describeInputs(const std::vector<std::string>& inputs, const BandSelection& sel) {
    const auto t0 = Clock::now();
    if (inputs.size() == 1) {
        const std::vector<MdVariable> vars = listVariables(inputs[0]);
        if (vars.size() > 1) {
            std::printf("%d variables (%.0f ms):\n", int(vars.size()), msSince(t0));
            for (const MdVariable& v : vars) std::printf("  %-12s %s\n    %s\n", v.name.c_str(), v.desc.c_str(), v.dataset.c_str());
            return 0;
        }
    }
    std::string err;
    auto info = openCube(inputs, sel, err);
    if (!info) {
        std::printf("FAIL: %s\n", err.c_str());
        return 1;
    }
    const double openMs = msSince(t0);
    std::printf("%s\n%d x %d, %s, %d layer(s), opened in %.0f ms%s\n", info->description.c_str(), info->width,
                info->height, info->dataType.c_str(), info->T(), openMs, info->remote ? " (remote)" : "");
    std::printf("CRS: %s%s%s\n", info->crsName.empty() ? "none" : info->crsName.c_str(),
                info->crsAuthority.empty() ? "" : "  ", info->crsAuthority.c_str());
    if (info->hasGeoTransform) {
        const auto& g = info->geoTransform;
        std::printf("origin %.10g, %.10g; pixel %.6g x %.6g\n", g[0], g[3], g[1], g[5]);
    }
    if (!info->mdLevels.empty())
        std::printf("levels: %d, shown: %s\n", int(info->mdLevels.size()), info->mdLevels[size_t(info->sel.band - 1)].c_str());
    const int T = info->T();
    CubeReader reader(info);
    const int px = info->width / 2, py = info->height / 2;
    const auto tr = Clock::now();
    for (int t = 0; t < T; ++t) {
        if (T > 14 && t == 6) {
            std::printf("  ...\n");
            t = T - 6;
        }
        float v = NAN;
        const bool ok = reader.readPixel(t, px, py, v);
        std::printf("  %-20s %s\n", info->layers[t].label.c_str(), ok ? std::to_string(v).c_str() : "(read failed)");
    }
    std::printf("centre pixel (%d, %d) read in %.0f ms\n", px, py, msSince(tr));
    return 0;
}

int runZeitSelfTest(const std::vector<std::string>& inputs, const BandSelection& sel, const std::string& python,
                    const std::string& bridge, int threads) {
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
    const fs::path rt = fs::u8path(platform::resourceDir()) / "runtime";
    cfg.python = python.empty() ? bundledPython(rt.u8string()) : python;
    cfg.bridge = bridge.empty() ? (rt / "janus_zeit_bridge.py").u8string() : bridge;
    cfg.bundled = python.empty();
    cfg.logPath = (fs::u8path(platform::appDataDir()) / "zeit.log").u8string();
    cfg.threads = threads;
    ZeitClient zeit;
    zeit.start(cfg);
    const auto tz = Clock::now();
    while (zeit.state() == ZeitClient::State::Starting && msSince(tz) < 120000)
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    if (zeit.state() != ZeitClient::State::Ready) return fail("zeit: " + zeit.error());
    std::printf("zeit: %s (Python %s) ready in %.0f ms, %d tool(s), %d threads\n", zeit.zeitVersion().c_str(),
                zeit.pythonVersion().c_str(), zeit.startupMs(), int(zeit.tools().size()), threads);

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

    // Tools on the series first; then those on result maps (the agreement of the maps of
    // dates the first ones made).
    std::vector<const ZeitTool*> order;
    for (const ZeitTool& tool : zeit.tools())
        if (!tool.layerInput) order.push_back(&tool);
    for (const ZeitTool& tool : zeit.tools())
        if (tool.layerInput) order.push_back(&tool);
    json dateMaps = json::array(); // {"name", "path"} of every output in years
    int ran = 0;
    for (const ZeitTool* toolp : order) {
        const ZeitTool& tool = *toolp;
        std::string why = toolApplicability(tool, *info);
        if (tool.layerInput && dateMaps.size() < 2) why = "fewer than two maps of dates were made";
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
        for (const ZeitParam& p : tool.params)
            if (p.type == "layers") params[p.id] = dateMaps;

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
            // "Copy Python": the Zeit calls the run made, as code.
            const std::string code = reply.result.value("python", json::object()).value("code", "");
            if (code.find("zeit.") == std::string::npos) return fail(tool.id + ": the pixel run should return its Zeit calls");
            std::printf("  python: %s\n", code.substr(0, code.find('(')).c_str());
            // The BFAST family draws its fitted model (least squares between the breaks):
            // placed at the series' dates, it explains more than the mean does (R2 > 0;
            // not much more where BFAST finds no break in a series that has one).
            if (tool.id.rfind("bfast", 0) == 0) {
                double se = 0, ss = 0, mean = 0;
                int n = 0, m = 0;
                for (float v : series)
                    if (std::isfinite(v)) mean += v, ++m;
                mean /= std::max(1, m);
                for (const json& o : reply.result.value("overlays", json::array())) {
                    const std::string label = o.value("label", "");
                    if (o.value("type", "") != "line" || label.find("trend") != std::string::npos ||
                        label.find("projected") != std::string::npos)
                        continue;
                    const json xs = o["x"], ys = o["y"];
                    for (int t = 0; t < info->T(); ++t) {
                        if (!std::isfinite(series[t])) continue;
                        for (size_t k = 0; k + 1 < xs.size(); ++k) // the model at the date (between two samples)
                            if (xs[k].get<double>() <= years[t] && years[t] <= xs[k + 1].get<double>() &&
                                ys[k].is_number() && ys[k + 1].is_number()) {
                                const double x0 = xs[k], x1 = xs[k + 1], f = x1 > x0 ? (years[t] - x0) / (x1 - x0) : 0;
                                const double y = ys[k].get<double>() * (1 - f) + ys[k + 1].get<double>() * f;
                                se += (y - series[t]) * (y - series[t]);
                                ss += (series[t] - mean) * (series[t] - mean);
                                ++n;
                                break;
                            }
                    }
                }
                if (n < 10) return fail(tool.id + ": the fitted model should be drawn over the series");
                const double r2 = 1 - se / std::max(ss, 1e-12);
                std::printf("  model on the chart: %d observations under it, R2 %.3f\n", n, r2);
                if (r2 < 0.05) return fail(tool.id + ": the fitted model does not follow the series");
            }
        }

        if (tool.raster) {
            json spec = {{"tool", tool.id}, {"params", params}};
            if (!tool.layerInput && !zeitJobInputs(*info, tool, roles, work.u8string(), "cube", spec, err))
                return fail("vrt: " + err);
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
            {
                // Its log: what ran, the progress, every output and the end, once the process is gone.
                const auto tw = Clock::now();
                std::string log;
                do {
                    std::this_thread::sleep_for(std::chrono::milliseconds(10));
                    std::ifstream f(fs::u8path(job->logPath));
                    log.assign(std::istreambuf_iterator<char>(f), std::istreambuf_iterator<char>());
                } while (log.find("] done (") == std::string::npos && msSince(tw) < 10000);
                size_t outputs = 0, lines = 0;
                for (size_t k = log.find("] output: "); k != std::string::npos; k = log.find("] output: ", k + 1)) ++outputs;
                for (char ch : log) lines += ch == '\n';
                std::printf("  log: %s (%zu lines)\n", job->logPath.c_str(), lines);
                if (log.find("tool: " + tool.id) == std::string::npos || log.find("%  ") == std::string::npos ||
                    outputs != activeOutputs(tool, params).size() ||
                    log.find("] done (process exit code 0)") == std::string::npos)
                    return fail(tool.id + ": the job's log should hold the tool, progress, every output and the end");
            }
            const json& outs = result["outputs"];
            if (outs.size() != activeOutputs(tool, params).size())
                return fail(tool.id + ": job returned a different set of outputs");
            for (const json& o : outs) {
                if (o.value("series", false)) {
                    // A series output opens as a series of the same dates (what Janus adds as a layer).
                    auto s = openCube({o.value("path", "")}, BandSelection{}, err);
                    if (!s) return fail(tool.id + ": series output: " + err);
                    if (s->T() != info->T() || s->timeIsDate != info->timeIsDate ||
                        (info->timeIsDate && s->layers.front().label != info->layers.front().label))
                        return fail(tool.id + ": series output " + o.value("id", "") + " has other dates");
                    std::printf("    %-28s series: %s, %s .. %s\n", o.value("name", "").c_str(), s->description.c_str(),
                                s->layers.front().label.c_str(), s->layers.back().label.c_str());
                    continue;
                }
                ResultLayer L;
                L.path = o.value("path", "");
                L.unit = o.value("unit", "");
                if (!loadResultRaster(L.path, 4096, L.data, L.tw, L.th, err)) return fail("load result: " + err);
                autoResultRange(L);
                size_t valid = 0;
                for (float v : L.data) valid += std::isfinite(v);
                std::printf("    %-28s %5.1f%% valid, range %.6g .. %.6g\n", o.value("name", "").c_str(),
                            100.0 * valid / L.data.size(), L.lo, L.hi);
                if (L.unit == "year" && valid > 0)
                    dateMaps.push_back({{"name", tool.name + ": " + o.value("name", "")}, {"path", L.path}});
                const size_t typical = o.value("class_series", json::array()).size();
                if (typical) std::printf("      typical series of %zu classes\n", typical);
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

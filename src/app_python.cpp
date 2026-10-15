// "Copy Python": code that reproduces outside Janus the series on the chart (the
// cursor's, a pin's or the ROI's), as Janus reads it (band or normalized
// difference, quality mask, nodata, scale/offset; the ROI's pixels whose centres
// are inside), its statistics (mean, OLS and Sen's slopes per year) and the
// Zeit tool fitted on the chart: the bridge records the calls the tool made
// (janus_zeit_bridge.run_recording) and Janus writes the data they need first.
// Needs numpy and rasterio (and xarray + zeit-cdts for the tool).
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <sstream>

#include <imgui_internal.h>

#include "platform.hpp"
#include "profiler.hpp"

namespace fs = std::filesystem;

namespace {

// A Python string literal (paths with backslashes, names with quotes).
std::string pyStr(const std::string& s) {
    std::string out = "\"";
    for (char c : s) {
        if (c == '\\' || c == '"') out += '\\';
        out += c;
    }
    return out + "\"";
}

std::string num(double v) {
    if (std::isnan(v)) return "float(\"nan\")";
    char b[40];
    std::snprintf(b, sizeof(b), "%.10g", v);
    return b;
}

std::string pyBand(int b) { return b > 0 ? std::to_string(b) : "None"; }

const char* kQaFunctions = R"(
def usable(q, rule):
    """Observations Janus keeps, from the quality band's codes (nodata: none)."""
    ok = np.isfinite(q) & (q >= 0)
    c = np.where(ok, q, 0).astype(np.int64)
    if rule == "fmask":          # 0 clear land, 1 water
        return ok & ((c == 0) | (c == 1))
    if rule == "landsat_c2":     # QA_PIXEL bits 0 fill, 1 dilated cloud, 3 cloud, 4 shadow, 5 snow
        return ok & ((c & 0x3B) == 0)
    return ok & (c != 0)         # a 0/1 mask
)";

} // namespace

std::string App::pythonSnippet(int which) {
    if (!s_) return "";
    const CubeInfo& info = *s_->info;
    const SeriesLayer* A = activeLayer();
    const int T = info.T();
    const SeriesView* view = nullptr;
    if (which == 0 && hover_.x >= 0) view = &hover_;
    for (const SeriesView& p : pins_)
        if (which > 0 && p.id == which) view = &p;
    const bool roi = which < 0 && roi_ && roi_->done.load() == T;
    if (!view && !roi) return "";
    std::ostringstream o;
    char today[16];
    const std::time_t now = std::time(nullptr);
    const std::tm tm = platform::localTime(now);
    std::strftime(today, sizeof(today), "%Y-%m-%d", &tm);
    std::string what = roi ? (roiPoly_.empty() ? "the ROI (a rectangle)" : "the ROI (a polygon)")
                           : view == &hover_ ? "the cursor's pixel" : "pin " + std::to_string(view->id);
    o << "\"\"\"Janus " << JANUS_VERSION << " (" << today << "): " << what << " of " << (A ? A->name : "the series")
      << ",\nreproduced outside Janus. Needs: pip install numpy rasterio";
    const json* zr = roi ? &roiZeitResult_ : &view->zeitResult;
    const bool tool = !pixelTool_.empty() && zr->is_object() && zr->contains("python");
    if (tool) o << " xarray zeit-cdts";
    o << "\n\"\"\"\nimport numpy as np\nimport rasterio\nfrom rasterio.windows import Window\n";
    if (roi && !roiPoly_.empty()) o << "from rasterio.features import rasterize\n";
    o << "from rasterio.enums import Resampling\n";

    // The series: each date's file and band (a spectrum: one file, a band per wavelength).
    const BandSelection& sel = info.sel;
    o << "\n# " << info.description << (info.selectionText().empty() ? "" : ": " + info.selectionText()) << "\n";
    o << "# Each date: (file, band, band B of a normalized difference (A - B) / (A + B), quality band).\n";
    o << "layers = [\n";
    for (int t = 0; t < T; ++t) {
        const Layer& L = info.layers[t];
        const int base = L.bandBase;
        o << "    (" << pyStr(L.path) << ", " << L.band << ", " << pyBand(sel.ndBand > 0 ? sel.ndBand + base : 0) << ", "
          << pyBand(sel.qaBand > 0 && sel.qaRule != QaRule::None ? sel.qaBand + base : 0) << "),\n";
    }
    o << "]\n";
    if (info.timeIsDate) {
        o << "dates = np.array([";
        for (int t = 0; t < T; ++t) o << (t ? ", " : "") << "\"" << info.layers[t].label.substr(0, 10) << "\"";
        o << "], dtype=\"datetime64[D]\")\n";
    }
    if (info.spectral) {
        o << "wavelengths_nm = np.array([";
        for (int t = 0; t < T; ++t) o << (t ? ", " : "") << num(info.layers[t].time);
        o << "])\n";
    }
    o << "# Time in years since the first date, as Janus fits trends (365.2425-day years).\n";
    o << "years = np.array([";
    for (int t = 0; t < T; ++t) o << (t ? ", " : "") << num(info.yearsFromStart(t));
    o << "])\n";
    o << "qa_rule = " << pyStr(qaRuleId(sel.qaRule)) << "\n";
    o << kQaFunctions;
    o << R"(

def read(path, band, window, out_shape=None, raw=False):
    """A band over a window: nodata -> NaN, the band's scale and offset applied."""
    with rasterio.open(path) as src:
        kw = {"out_shape": out_shape, "resampling": Resampling.nearest} if out_shape else {}
        a = src.read(band, window=window, masked=True, **kw).astype("float64")
        if not raw:
            a = a * src.scales[band - 1] + src.offsets[band - 1]
        return np.ma.filled(a, np.nan)


def shown(layer, window, out_shape=None):
    """What the series shows at a date: the band, or the normalized difference, masked by the quality band."""
    path, band, nd_band, qa_band = layer
    v = read(path, band, window, out_shape)
    if nd_band:
        b = read(path, nd_band, window, out_shape)
        with np.errstate(invalid="ignore", divide="ignore"):
            v = np.where(v + b != 0, (v - b) / (v + b), np.nan)
    if qa_band:
        v = np.where(usable(read(path, qa_band, window, out_shape, raw=True), qa_rule), v, np.nan)
    return v
)";
    double gx = 0, gy = 0;
    if (roi) {
        const RoiData& R = *roi_;
        const int w = R.x1 - R.x0, h = R.y1 - R.y0;
        o << "\n# The ROI: the window x " << R.x0 << ".." << R.x1 << ", y " << R.y0 << ".." << R.y1 << " of the files' pixels";
        if (R.sampled) o << ",\n# read at " << R.bw << " x " << R.bh << " (Janus subsamples ROIs above 4 Mpx, nearest neighbour)";
        o << ".\nwindow = Window(" << R.x0 << ", " << R.y0 << ", " << w << ", " << h << ")\n";
        o << "shape = (" << R.bh << ", " << R.bw << ")\n";
        if (!roiPoly_.empty()) {
            // The polygon in the window's cells: pixels whose centres are inside count.
            const double kx = double(R.bw) / w, ky = double(R.bh) / h;
            o << "# Its polygon (rings in the window's cells, even-odd: holes are rings inside others);\n"
                 "# a cell counts when its centre is inside.\n";
            o << "rings = [\n";
            for (const Ring& r : roiPoly_) {
                o << "    [";
                for (size_t i = 0; i + 1 < r.size(); i += 2)
                    o << (i ? ", " : "") << "(" << num((r[i] - R.x0) * kx) << ", " << num((r[i + 1] - R.y0) * ky) << ")";
                o << "],\n";
            }
            o << "]\n";
            o << "inside = np.zeros(shape, dtype=bool)\n"
                 "for ring in rings:  # each ring flips what it covers (even-odd)\n"
                 "    inside ^= rasterize([{\"type\": \"Polygon\", \"coordinates\": [ring + ring[:1]]}], out_shape=shape,\n"
                 "                        fill=0, default_value=1, dtype=\"uint8\").astype(bool)\n";
        } else {
            o << "inside = np.ones(shape, dtype=bool)\n";
        }
        o << R"(
mean, p10, p90 = (np.full(len(layers), np.nan) for _ in range(3))
for t, layer in enumerate(layers):
    v = shown(layer, window, shape)[inside]
    v = v[np.isfinite(v)]
    if v.size:
        mean[t] = v.mean()
        p10[t], p90[t] = np.percentile(v, [10, 90])  # numpy's percentiles: Janus' may differ slightly
values = mean
)";
    } else {
        o << "\n# The pixel (column, row of the files)";
        if (info.pixelToGeo(view->x + 0.5, view->y + 0.5, gx, gy))
            o << ", its centre at x " << num(gx) << ", y " << num(gy) << " ("
              << (info.crsAuthority.empty() ? info.crsName : info.crsAuthority) << ")";
        o << ".\ncol, row = " << view->x << ", " << view->y << "\n";
        o << "values = np.array([shown(layer, Window(col, row, 1, 1))[0, 0] for layer in layers])\n";
    }
    // The Statistics panel's numbers.
    o << R"(
ok = np.isfinite(values)
y, t = values[ok], years[ok]
print("n", ok.sum(), "mean", y.mean(), "std", y.std(ddof=1) if y.size > 1 else np.nan)
if y.size >= 2:
    slope, intercept = np.polyfit(t, y, 1)                  # OLS, per year
    i, j = np.triu_indices(y.size, 1)
    keep = t[j] != t[i]
    sen = np.median((y[j] - y[i])[keep] / (t[j] - t[i])[keep])  # Sen's slope, per year
    print("OLS slope", slope, "Sen's slope", sen)
)";
    if (tool) {
        const json& py = (*zr)["python"];
        const std::vector<std::string> inputs = py.value("inputs", std::vector<std::string>());
        auto uses = [&](const char* n) { return std::find(inputs.begin(), inputs.end(), n) != inputs.end(); };
        const ZeitTool* zt = zeit_ ? zeit_->tool(pixelTool_) : nullptr;
        o << "\n# " << (zt ? zt->name : pixelTool_) << ", fitted on the chart with these parameters: the calls Janus' Zeit\n"
             "# bridge made (zeit_bridge/tool_*.py turns their results into the chart's lines and table).\n";
        o << "import xarray as xr\nimport zeit\n\n";
        const std::string time = info.timeIsDate ? "dates.astype(\"datetime64[ns]\")" : "np.arange(len(values))";
        if (uses("series"))
            o << "series = xr.DataArray(values[:, None, None], dims=(\"time\", \"y\", \"x\"), coords={\"time\": " << time
              << "})\n";
        if (uses("bands_cube") || uses("fmask")) {
            // Multiband tools (CCDC...): the role bands of the pixel, in reflectance x 10000, and Fmask codes.
            const BandRoles& roles = activeBandRoles();
            o << "# The bands by role (bands of each date's file), as reflectance x 10000 (Zeit's convention:\n"
                 "# 0-1 reflectance x 10000; Landsat C2 L2 numbers x 0.0000275 - 0.2, x 10000; set `units`).\n";
            o << "roles = {";
            bool first = true;
            for (const auto& [role, b] : roles) {
                o << (first ? "" : ", ") << pyStr(role) << ": " << b;
                first = false;
            }
            o << "}\n";
            o << "order = [r for r in (\"blue\", \"green\", \"red\", \"nir\", \"swir1\", \"swir2\", \"thermal\") if r in roles]\n";
            o << "raw = {r: np.array([read(layer[0], roles[r], Window(col, row, 1, 1))[0, 0] for layer in layers])\n"
                 "       for r in order}\n";
            o << "optical = np.concatenate([raw[r] for r in order if r != \"thermal\"])\n"
                 "med = np.nanmedian(optical)\n"
                 "units = \"unit\" if med <= 2 else \"landsat_c2\" if med >= 6500 else \"x10000\"\n"
                 "to_x10000 = {\"unit\": lambda v: v * 10000, \"landsat_c2\": lambda v: (v * 2.75e-5 - 0.2) * 10000,\n"
                 "             \"x10000\": lambda v: v}[units]\n"
                 "cube = np.stack([to_x10000(raw[r]) for r in order])  # (band, time); the thermal band: deg C x 100 in Zeit\n"
                 "bands_cube = xr.DataArray(cube.T[:, :, None, None], dims=(\"time\", \"band\", \"y\", \"x\"),\n"
                 "                          coords={\"time\": "
              << time << ", \"band\": order})\n";
            o << "# Fmask codes (0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 no observation) from the quality band.\n"
                 "complete = np.all(np.isfinite(cube), axis=0)\n";
            if (sel.qaBand > 0 && sel.qaRule != QaRule::None)
                o << "q = np.array([read(path, qa, Window(col, row, 1, 1), raw=True)[0, 0] for path, _, _, qa in layers])\n"
                     "c = np.where(np.isfinite(q) & (q >= 0), q, 255).astype(np.int64)\n"
                     "if qa_rule == \"fmask\":\n"
                     "    f = c.astype(np.int32)\n"
                     "elif qa_rule == \"landsat_c2\":\n"
                     "    f = np.zeros(c.shape, np.int32)\n"
                     "    f[(c & 128) != 0] = 1; f[(c & 32) != 0] = 3; f[(c & 16) != 0] = 2; f[(c & 10) != 0] = 4; f[(c & 1) != 0] = 255\n"
                     "else:\n"
                     "    f = np.where(c != 0, 0, 4).astype(np.int32)\n"
                     "fmask_codes = np.where(complete, f, 255)\n";
            else
                o << "fmask_codes = np.where(complete, 0, 255)  # no quality band: every complete observation is clear\n";
            o << "fmask = fmask_codes.astype(np.int32)[:, None, None]\n";
        }
        o << py.value("code", "") << "\n";
        o << "print(result1)\n";
    }
    return o.str();
}

void App::uiCopyPython() {
    PROFILE_ZONE();
    const bool any = hover_.x >= 0 || !pins_.empty() || (roi_ && roi_->done.load() == s_->info->T());
    ImGui::BeginDisabled(!any);
    if (ImGui::Button("Copy Python")) ImGui::OpenPopup("##copypython");
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("Python (numpy, rasterio; xarray and zeit for a tool) that reads the same series\n"
                          "from the files, computes its statistics and, with a Zeit tool on the chart,\n"
                          "makes the same Zeit calls with the same parameters");
    if (!ImGui::BeginPopup("##copypython")) return;
    auto item = [&](const std::string& label, int which) {
        if (ImGui::MenuItem(label.c_str())) {
            pythonCopied_ = pythonSnippet(which);
            ImGui::SetClipboardText(pythonCopied_.c_str());
        }
    };
    if (hover_.x >= 0) item("Cursor (" + std::to_string(hover_.x) + ", " + std::to_string(hover_.y) + ")", 0);
    for (const SeriesView& p : pins_)
        item("Pin " + std::to_string(p.id) + " (" + std::to_string(p.x) + ", " + std::to_string(p.y) + ")", p.id);
    if (roi_ && roi_->done.load() == s_->info->T()) item(roiPoly_.empty() ? "ROI (rectangle)" : "ROI (polygon)", -1);
    if (!pixelTool_.empty())
        ImGui::TextDisabled("with %s", zeit_ && zeit_->tool(pixelTool_) ? zeit_->tool(pixelTool_)->name.c_str() : "");
    ImGui::EndPopup();
}

// ---------------------------------------------------------------------------
// --selftest-ui stages 150-153: C (a band per date masked by Fmask) with a pin
// on the cloudy half and Mann-Kendall on the chart, then a polygonal ROI: the
// code copied is run by the bundled Python and its series must be Janus'.
// ---------------------------------------------------------------------------

int App::selfTestPython() {
    auto now = [] { return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count(); };
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    // Runs the code and reads the series it prints; false (and what it said) when it fails.
    auto run = [&](const std::string& code, std::vector<double>& out) -> bool {
        const fs::path dir = fs::temp_directory_path() / "janus-selftest-python";
        std::error_code ec;
        fs::create_directories(dir, ec);
        const std::string script = (dir / "snippet.py").u8string(), log = (dir / "out.txt").u8string();
        {
            std::ofstream f(fs::u8path(script), std::ios::binary);
            f << code
              << "\nimport json, math\n"
                 "print(\"JANUS_CHECK \" + json.dumps([float(v) if math.isfinite(v) else None for v in values]))\n";
        }
        platform::ChildProcess p;
        std::string err;
        if (!p.start({zeitConfig().python, "-I", script}, log, err)) {
            std::printf("    %s\n", err.c_str());
            return false;
        }
        const int rc = p.finish();
        std::ifstream f(fs::u8path(log), std::ios::binary);
        const std::string text((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
        const size_t k = text.find("JANUS_CHECK ");
        if (rc != 0 || k == std::string::npos) {
            std::printf("    exit code %d:\n%s\n", rc, text.c_str());
            return false;
        }
        const json j = json::parse(text.substr(k + 12, text.find('\n', k) - k - 12));
        out.clear();
        for (const json& v : j) out.push_back(v.is_number() ? v.get<double>() : NAN);
        if (platform::getEnv("JANUS_KEEP_PYTHON").empty()) fs::remove_all(dir, ec); // else kept to read
        return true;
    };
    auto same = [](const std::vector<float>& a, const std::vector<double>& b, double tol, int& nan) {
        nan = 0;
        if (a.size() != b.size()) return false;
        for (size_t t = 0; t < a.size(); ++t) {
            if (std::isnan(a[t]) != std::isnan(b[t])) return false;
            nan += std::isnan(a[t]);
            if (!std::isnan(a[t]) && std::fabs(a[t] - b[t]) > tol * (1 + std::fabs(b[t]))) return false;
        }
        return true;
    };
    switch (st_.stage) {
    case 150: {
        if (!zeit_ || zeit_->state() != ZeitClient::State::Ready || !zeit_->tool("mann_kendall")) {
            if (now() - st_.since > 60) return fail("Zeit should be ready (Mann-Kendall)");
            break;
        }
        clearPins();
        addPin(150, 30); // the right half: clouds every third date, masked by Fmask
        const ZeitTool& mk = *zeit_->tool("mann_kendall");
        ToolUi& ui = toolUi_[mk.id];
        ui.params = json::object();
        for (const ZeitParam& p : mk.params) ui.params[p.id] = p.def;
        pixelTool_ = mk.id;
        ++pixelVersion_;
        next("C: a pin on the cloudy half, Mann-Kendall on the chart");
        break;
    }
    case 151: {
        const SeriesView& pin = pins_.back();
        if (!pin.exact || !pin.zeitResult.is_object() || !pin.zeitResult.contains("python")) break;
        const std::string code = pythonSnippet(pin.id);
        std::vector<double> v;
        if (!run(code, v)) return fail("the pin's code should run");
        int nan = 0;
        if (!same(pin.values, v, 1e-6, nan)) return fail("the pin's code should read the pin's series");
        if (code.find("zeit.mann_kendall(series") == std::string::npos) return fail("the code should call Mann-Kendall");
        std::printf("    pin: %zu dates as in Janus (%d masked by Fmask), zeit.mann_kendall(series, ...) ran\n",
                    v.size(), nan);
        if (nan == 0) return fail("the pin should have dates masked by the quality band");
        pixelTool_.clear();
        ++pixelVersion_;
        static const double kL[] = {20, 10, 120, 10, 120, 40, 60.4, 40, 60.4, 100, 20, 100};
        Ring r(std::begin(kL), std::end(kL));
        if (!setRoiPolygon({r})) return fail("the ROI polygon");
        next("the pin's code: the same series, and the tool's call");
        break;
    }
    case 152: {
        const int T = s_->info->T();
        if (!roi_ || roi_->done.load() < T || roiSeen_ != T) break;
        std::vector<double> v;
        if (!run(pythonSnippet(-1), v)) return fail("the ROI's code should run");
        int nan = 0;
        if (!same(roiMean_, v, 1e-5, nan)) return fail("the ROI's code should give the ROI's mean at every date");
        std::printf("    ROI (polygon): the mean of %d dates as in Janus\n", T);
        clearRoi();
        clearPins();
        next("the ROI's code: the mean inside the polygon");
        // A multiband tool: CCDC on the Zeit self-test's Landsat series, when it is there.
        std::error_code ec;
        st_.dir = (fs::u8path(lastInputs_.empty() ? std::string() : lastInputs_[0]).parent_path() / "landsat").u8string();
        if (lastInputs_.empty() || !fs::is_directory(fs::u8path(st_.dir), ec)) {
            st_.stage = 160; // NetCDF (app_open.cpp), which prints OK
            st_.since = now();
            return -1;
        }
        openInputs({st_.dir});
        break;
    }
    case 153: {
        if (opening_.valid() || layers_.empty() || s_->info->bandsPerDate < 6 || !s_->overview.complete()) break;
        clearPins();
        addPin(s_->info->width / 2, s_->info->height / 2);
        const ZeitTool* ccdc = zeit_ ? zeit_->tool("ccdc") : nullptr;
        if (!ccdc) return fail("CCDC should be among the tools");
        ToolUi& ui = toolUi_[ccdc->id];
        ui.params = json::object();
        for (const ZeitParam& p : ccdc->params) ui.params[p.id] = p.def;
        pixelTool_ = ccdc->id;
        ++pixelVersion_;
        next("the Landsat series (six bands and Fmask per date), CCDC on a pin");
        break;
    }
    case 154: {
        const SeriesView& pin = pins_.back();
        if (!pin.exact || !pin.zeitResult.is_object() || !pin.zeitResult.contains("python")) break;
        const std::string code = pythonSnippet(pin.id);
        std::vector<double> v;
        if (!run(code + "\nassert int(result1[\"n_segments\"].values.ravel()[0]) >= 1\n", v))
            return fail("the CCDC code should run and fit segments");
        int nan = 0;
        if (!same(pin.values, v, 1e-6, nan)) return fail("the CCDC code should read the pin's series");
        if (code.find("zeit.ccdc(bands_cube, qa=fmask") == std::string::npos)
            return fail("the code should call zeit.ccdc on the bands and their Fmask codes");
        std::printf("    CCDC: the bands by role in reflectance x 10000 and the Fmask codes; zeit.ccdc ran\n");
        pixelTool_.clear();
        ++pixelVersion_;
        clearPins();
        next("the CCDC code: its bands, Fmask codes and call");
        st_.stage = 160; // NetCDF (app_open.cpp), which prints OK
        return -1;
    }
    }
    return -1;
}

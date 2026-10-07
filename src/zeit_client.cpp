#include "zeit_client.hpp"

#include <cmath>
#include <ctime>
#include <filesystem>
#include <fstream>

#include "gl.hpp" // glfwPostEmptyEvent: wake the UI loop when a message arrives

#ifdef _WIN32
#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

namespace fs = std::filesystem;

namespace {

#ifdef _WIN32
std::wstring toWide(const std::string& s) {
    int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return {};
    std::wstring out(size_t(len - 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, out.data(), len);
    return out;
}

std::wstring quoteArg(const std::wstring& a) {
    if (!a.empty() && a.find_first_of(L" \t\"") == std::wstring::npos) return a;
    std::wstring out = L"\"";
    int backslashes = 0;
    for (wchar_t c : a) {
        if (c == L'\\') {
            ++backslashes;
        } else {
            if (c == L'"') out.append(size_t(backslashes) * 2 + 1, L'\\');
            else out.append(size_t(backslashes), L'\\');
            backslashes = 0;
            out += c;
        }
    }
    out.append(size_t(backslashes) * 2, L'\\');
    return out + L"\"";
}

// Environment for the child. For the bundled runtime, drop variables that would
// point Python or GDAL/PROJ at another installation on this machine.
std::vector<wchar_t> childEnvironment(bool bundled) {
    static const wchar_t* drop[] = {L"PYTHONHOME", L"PYTHONPATH", L"PYTHONSTARTUP", L"GDAL_DATA", L"GDAL_DRIVER_PATH",
                                    L"PROJ_LIB", L"PROJ_DATA", L"CONDA_PREFIX"};
    std::vector<wchar_t> block;
    LPWCH env = GetEnvironmentStringsW();
    for (LPWCH p = env; *p; p += wcslen(p) + 1) {
        std::wstring kv = p;
        bool skip = false;
        if (bundled) {
            const std::wstring key = kv.substr(0, kv.find(L'='));
            for (const wchar_t* d : drop)
                if (_wcsicmp(key.c_str(), d) == 0) skip = true;
        }
        if (_wcsnicmp(kv.c_str(), L"PYTHONUTF8=", 11) == 0 || _wcsnicmp(kv.c_str(), L"PYTHONNOUSERSITE=", 17) == 0)
            skip = true;
        if (!skip) block.insert(block.end(), kv.c_str(), kv.c_str() + kv.size() + 1);
    }
    FreeEnvironmentStringsW(env);
    for (const wchar_t* extra : {L"PYTHONUTF8=1", L"PYTHONNOUSERSITE=1"})
        block.insert(block.end(), extra, extra + wcslen(extra) + 1);
    block.push_back(L'\0');
    return block;
}
#endif

void logLine(const std::string& path, const std::string& text) {
    std::ofstream f(fs::u8path(path), std::ios::app);
    const std::time_t now = std::time(nullptr);
    std::tm tm{};
    localtime_s(&tm, &now);
    char ts[32];
    std::strftime(ts, sizeof(ts), "%Y-%m-%d %H:%M:%S", &tm);
    f << "=== " << ts << " " << text << "\n";
}

} // namespace

// ---------------------------------------------------------------------------
// ZeitProcess
// ---------------------------------------------------------------------------

ZeitProcess::~ZeitProcess() {
#ifdef _WIN32
    if (stdinWrite_) {
        CloseHandle(stdinWrite_); // EOF on stdin: the serve loop exits by itself
        stdinWrite_ = nullptr;
    }
    if (process_ && WaitForSingleObject(process_, 2000) != WAIT_OBJECT_0) TerminateProcess(process_, 1);
    if (reader_.joinable()) reader_.join();
    if (stdoutRead_) CloseHandle(stdoutRead_);
    if (process_) CloseHandle(process_);
#endif
}

bool ZeitProcess::start(const ZeitConfig& cfg, const std::vector<std::string>& args, std::string& error) {
#ifdef _WIN32
    SECURITY_ATTRIBUTES sa{sizeof(sa), nullptr, TRUE};
    HANDLE inR = nullptr, inW = nullptr, outR = nullptr, outW = nullptr;
    if (!CreatePipe(&inR, &inW, &sa, 0) || !CreatePipe(&outR, &outW, &sa, 0)) {
        error = "CreatePipe failed";
        return false;
    }
    SetHandleInformation(inW, HANDLE_FLAG_INHERIT, 0);
    SetHandleInformation(outR, HANDLE_FLAG_INHERIT, 0);
    HANDLE logH = CreateFileW(toWide(cfg.logPath).c_str(), FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa,
                              OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (logH == INVALID_HANDLE_VALUE)
        logH = CreateFileW(L"NUL", GENERIC_WRITE, FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, nullptr);

    // Inherit exactly these three handles, nothing else from this process.
    HANDLE inherit[3] = {inR, outW, logH};
    SIZE_T attrSize = 0;
    InitializeProcThreadAttributeList(nullptr, 1, 0, &attrSize);
    std::vector<char> attrBuf(attrSize);
    auto attrs = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(attrBuf.data());
    InitializeProcThreadAttributeList(attrs, 1, 0, &attrSize);
    UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, inherit, sizeof(inherit), nullptr, nullptr);

    STARTUPINFOEXW si{};
    si.StartupInfo.cb = sizeof(si);
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = inR;
    si.StartupInfo.hStdOutput = outW;
    si.StartupInfo.hStdError = logH;
    si.lpAttributeList = attrs;

    std::wstring cmd = quoteArg(toWide(cfg.python)) + L" -X utf8 " + quoteArg(toWide(cfg.bridge));
    for (const std::string& a : args) cmd += L" " + quoteArg(toWide(a));
    std::vector<wchar_t> env = childEnvironment(cfg.bundled);
    PROCESS_INFORMATION pi{};
    const BOOL ok = CreateProcessW(nullptr, cmd.data(), nullptr, nullptr, TRUE,
                                   CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                                   env.data(), nullptr, &si.StartupInfo, &pi);
    DeleteProcThreadAttributeList(attrs);
    CloseHandle(inR);
    CloseHandle(outW);
    CloseHandle(logH);
    if (!ok) {
        error = "could not start " + cfg.python + " (error " + std::to_string(GetLastError()) + ")";
        CloseHandle(inW);
        CloseHandle(outR);
        return false;
    }
    CloseHandle(pi.hThread);
    process_ = pi.hProcess;
    stdinWrite_ = inW;
    stdoutRead_ = outR;
    reader_ = std::thread([this] { readLoop(); });
    return true;
#else
    error = "Zeit integration is only implemented on Windows";
    return false;
#endif
}

void ZeitProcess::readLoop() {
#ifdef _WIN32
    std::string buf;
    char chunk[65536];
    DWORD n = 0;
    while (ReadFile(stdoutRead_, chunk, sizeof(chunk), &n, nullptr) && n > 0) {
        buf.append(chunk, n);
        size_t pos;
        while ((pos = buf.find('\n')) != std::string::npos) {
            std::string line = buf.substr(0, pos);
            buf.erase(0, pos + 1);
            if (!line.empty() && line.back() == '\r') line.pop_back();
            if (!line.empty() && onLine) onLine(line);
        }
    }
    if (process_) WaitForSingleObject(process_, 5000);
    if (onExit) onExit();
#endif
}

bool ZeitProcess::writeLine(const std::string& line) {
#ifdef _WIN32
    if (!stdinWrite_) return false;
    const std::string data = line + "\n";
    DWORD written = 0;
    return WriteFile(stdinWrite_, data.data(), DWORD(data.size()), &written, nullptr) && written == data.size();
#else
    return false;
#endif
}

void ZeitProcess::terminate() {
#ifdef _WIN32
    if (process_) TerminateProcess(process_, 1);
#endif
}

bool ZeitProcess::running() const {
#ifdef _WIN32
    return process_ && WaitForSingleObject(process_, 0) == WAIT_TIMEOUT;
#else
    return false;
#endif
}

int ZeitProcess::exitCode() const {
#ifdef _WIN32
    DWORD code = 0;
    if (process_ && GetExitCodeProcess(process_, &code)) return int(code);
#endif
    return -1;
}

void ZeitJob::cancel() {
    State expected = State::Running;
    if (state.compare_exchange_strong(expected, State::Cancelled) && proc) proc->terminate();
}

// ---------------------------------------------------------------------------
// ZeitClient
// ---------------------------------------------------------------------------

ZeitClient::~ZeitClient() {
    shuttingDown_ = true;
    if (proc_ && state_ == State::Ready) proc_->writeLine(R"({"id":0,"method":"shutdown"})");
    proc_.reset();
}

void ZeitClient::start(const ZeitConfig& cfg) {
    if (state_ != State::Off) return;
    cfg_ = cfg;
    t0_ = std::chrono::steady_clock::now();
    std::error_code ec;
    if (!fs::exists(fs::u8path(cfg.python), ec) || !fs::exists(fs::u8path(cfg.bridge), ec)) {
        std::lock_guard<std::mutex> lk(m_);
        error_ = "Zeit runtime not found (" + cfg.python + ")";
        state_ = State::Failed;
        return;
    }
    state_ = State::Starting;
    logLine(cfg.logPath, "serve: " + cfg.python + " " + cfg.bridge);
    proc_ = std::make_unique<ZeitProcess>();
    proc_->onLine = [this](const std::string& l) { onLine(l); };
    ZeitProcess* p = proc_.get(); // proc_ is already null while it is being destroyed
    proc_->onExit = [this, p] {
        if (shuttingDown_) return;
        std::lock_guard<std::mutex> lk(m_);
        error_ = "the Zeit bridge stopped (exit code " + std::to_string(p->exitCode()) + "); see " + cfg_.logPath;
        state_ = State::Failed;
        glfwPostEmptyEvent();
    };
    std::string err;
    if (!proc_->start(cfg, {"serve"}, err)) {
        std::lock_guard<std::mutex> lk(m_);
        error_ = err;
        state_ = State::Failed;
        return;
    }
    proc_->writeLine(R"({"id":1,"method":"hello"})");
}

std::string ZeitClient::error() const {
    std::lock_guard<std::mutex> lk(m_);
    return error_;
}

std::string ZeitClient::zeitVersion() const {
    std::lock_guard<std::mutex> lk(m_);
    return zeitVersion_;
}

std::string ZeitClient::pythonVersion() const {
    std::lock_guard<std::mutex> lk(m_);
    return pythonVersion_;
}

const ZeitTool* ZeitClient::tool(const std::string& id) const {
    if (state_ != State::Ready) return nullptr;
    for (const ZeitTool& t : tools_)
        if (t.id == id) return &t;
    return nullptr;
}

static std::vector<ZeitTool> parseTools(const json& arr) {
    std::vector<ZeitTool> out;
    for (const json& t : arr) {
        ZeitTool tool;
        tool.id = t.value("id", "");
        tool.name = t.value("name", tool.id);
        tool.category = t.value("category", "Tools");
        tool.description = t.value("description", "");
        if (t.contains("requires")) {
            tool.requiresTime = t["requires"].value("time", "any");
            tool.minDates = t["requires"].value("min_dates", 0);
            tool.minPerYear = t["requires"].value("min_per_year", 0);
        }
        for (const json& m : t.value("modes", json::array())) {
            if (m == "pixel") tool.pixel = true;
            if (m == "raster") tool.raster = true;
        }
        for (const json& p : t.value("params", json::array())) {
            ZeitParam zp;
            zp.id = p.value("id", "");
            zp.label = p.value("label", zp.id);
            zp.type = p.value("type", "float");
            zp.help = p.value("help", "");
            zp.def = p.value("default", json());
            zp.min = p.value("min", 0.0);
            zp.max = p.value("max", 0.0);
            zp.options = p.value("options", std::vector<std::string>());
            zp.labels = p.value("labels", zp.options);
            tool.params.push_back(zp);
        }
        for (const json& o : t.value("outputs", json::array()))
            tool.outputs.push_back({o.value("id", ""), o.value("name", ""), o.value("colormap", "Viridis"),
                                    o.value("unit", "")});
        out.push_back(tool);
    }
    return out;
}

void ZeitClient::onLine(const std::string& line) {
    json msg = json::parse(line, nullptr, false);
    if (msg.is_discarded() || !msg.is_object() || msg.contains("event")) return;
    const uint64_t id = msg.value("id", uint64_t(0));
    if (id == 1) {
        std::lock_guard<std::mutex> lk(m_);
        if (msg.contains("result")) {
            const json& r = msg["result"];
            zeitVersion_ = r.value("zeit_version", "?");
            pythonVersion_ = r.value("python", "?");
            tools_ = parseTools(r.value("tools", json::array()));
            startupMs_ = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0_).count();
            state_ = State::Ready;
        } else {
            error_ = msg.value("error", "hello failed");
            state_ = State::Failed;
        }
    } else if (id != 0) {
        PixelReply r{id, msg.contains("result"), msg.value("result", json()), msg.value("error", "")};
        std::lock_guard<std::mutex> lk(m_);
        replies_.push_back(std::move(r));
    }
    glfwPostEmptyEvent();
}

uint64_t ZeitClient::runPixel(const std::string& toolId, const json& params, const std::vector<double>& years,
                              const std::vector<float>& values) {
    if (state_ != State::Ready || !proc_) return 0;
    const uint64_t id = nextId_++;
    json vals = json::array();
    for (float v : values) vals.push_back(std::isfinite(v) ? json(double(v)) : json());
    const json req = {{"id", id},
                      {"method", "run_pixel"},
                      {"params", {{"tool", toolId}, {"params", params}, {"years", years}, {"values", vals}}}};
    return proc_->writeLine(req.dump()) ? id : 0;
}

std::vector<PixelReply> ZeitClient::takeReplies() {
    std::lock_guard<std::mutex> lk(m_);
    std::vector<PixelReply> out;
    out.swap(replies_);
    return out;
}

std::shared_ptr<ZeitJob> ZeitClient::startJob(const json& spec, const std::string& specPath, const std::string& title) {
    auto job = std::make_shared<ZeitJob>();
    job->title = title;
    job->toolId = spec.value("tool", "");
    job->outputDir = spec.value("output_dir", "");
    {
        std::ofstream f(fs::u8path(specPath), std::ios::binary);
        f << spec.dump(2);
    }
    logLine(cfg_.logPath, "job: " + title + " (" + specPath + ")");
    job->proc = std::make_unique<ZeitProcess>();
    ZeitJob* j = job.get(); // the process (and its callbacks) is owned by the job
    job->proc->onLine = [j](const std::string& line) {
        json msg = json::parse(line, nullptr, false);
        if (msg.is_discarded() || !msg.is_object()) return;
        if (msg.contains("progress")) {
            j->progress = msg.value("progress", 0.0);
            std::lock_guard<std::mutex> lk(j->m);
            j->message = msg.value("message", "");
        } else if (msg.contains("result")) {
            {
                std::lock_guard<std::mutex> lk(j->m);
                j->result = msg["result"];
            }
            j->progress = 1.0;
            ZeitJob::State expected = ZeitJob::State::Running;
            j->state.compare_exchange_strong(expected, ZeitJob::State::Done);
        } else if (msg.contains("error")) {
            {
                std::lock_guard<std::mutex> lk(j->m);
                j->error = msg.value("error", "error");
            }
            ZeitJob::State expected = ZeitJob::State::Running;
            j->state.compare_exchange_strong(expected, ZeitJob::State::Failed);
        }
        glfwPostEmptyEvent();
    };
    const std::string logPath = cfg_.logPath;
    job->proc->onExit = [j, logPath] {
        j->seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - j->started).count();
        ZeitJob::State expected = ZeitJob::State::Running;
        if (j->state.compare_exchange_strong(expected, ZeitJob::State::Failed)) {
            std::lock_guard<std::mutex> lk(j->m);
            j->error = "the job process exited (code " + std::to_string(j->proc->exitCode()) + "); see " + logPath;
        }
        glfwPostEmptyEvent();
    };
    std::string err;
    if (!job->proc->start(cfg_, {"job", specPath}, err)) {
        std::lock_guard<std::mutex> lk(job->m);
        job->error = err;
        job->state = ZeitJob::State::Failed;
    }
    return job;
}

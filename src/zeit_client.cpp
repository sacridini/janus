#include "zeit_client.hpp"

#include <cmath>
#include <ctime>
#include <filesystem>
#include <fstream>

#include "platform.hpp"
#include "gl.hpp" // glfwPostEmptyEvent: wake the UI loop when a message arrives

#ifdef _WIN32
#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <cerrno>
#include <csignal>
#include <cstring>
#include <fcntl.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>
extern char** environ;
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
#else
// Same rules as the Windows version, as "KEY=value" strings.
std::vector<std::string> childEnvironment(bool bundled) {
    static const char* drop[] = {"PYTHONHOME=", "PYTHONPATH=", "PYTHONSTARTUP=", "GDAL_DATA=", "GDAL_DRIVER_PATH=",
                                 "PROJ_LIB=", "PROJ_DATA=", "CONDA_PREFIX=", "PYTHONUTF8=", "PYTHONNOUSERSITE="};
    std::vector<std::string> env;
    for (char** e = environ; *e; ++e) {
        const std::string kv = *e;
        bool skip = false;
        for (const char* d : drop) {
            const bool alwaysDropped = std::strncmp(d, "PYTHONUTF8", 10) == 0 || std::strncmp(d, "PYTHONNOUSERSITE", 16) == 0;
            if ((bundled || alwaysDropped) && kv.rfind(d, 0) == 0) skip = true;
        }
        if (!skip) env.push_back(kv);
    }
    env.push_back("PYTHONUTF8=1");
    env.push_back("PYTHONNOUSERSITE=1");
    return env;
}

bool makePipe(int fds[2]) {
    if (pipe(fds) != 0) return false;
    for (int i = 0; i < 2; ++i) fcntl(fds[i], F_SETFD, FD_CLOEXEC); // only the dup2'ed ends reach the child
    return true;
}
#endif

void logLine(const std::string& path, const std::string& text) {
    std::ofstream f(fs::u8path(path), std::ios::app);
    const std::time_t now = std::time(nullptr);
    const std::tm tm = platform::localTime(now);
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
#else
    if (stdinFd_ >= 0) {
        close(stdinFd_); // EOF on stdin: the serve loop exits by itself
        stdinFd_ = -1;
    }
    if (pid_ > 0) {
        for (int i = 0; i < 200 && !reap(false); ++i) usleep(10000); // up to 2 s
        if (!reap(false)) {
            kill(pid_, SIGKILL);
            reap(true);
        }
    }
    if (reader_.joinable()) reader_.join();
    if (stdoutFd_ >= 0) close(stdoutFd_);
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
    int in[2], out[2];
    if (!makePipe(in) || !makePipe(out)) {
        error = std::string("pipe failed: ") + std::strerror(errno);
        return false;
    }
    int logFd = open(cfg.logPath.c_str(), O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
    if (logFd < 0) logFd = open("/dev/null", O_WRONLY | O_CLOEXEC);

    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, in[0], 0);
    posix_spawn_file_actions_adddup2(&fa, out[1], 1);
    posix_spawn_file_actions_adddup2(&fa, logFd, 2);

    std::vector<std::string> argv = {cfg.python, "-X", "utf8", cfg.bridge};
    argv.insert(argv.end(), args.begin(), args.end());
    std::vector<char*> av;
    for (std::string& a : argv) av.push_back(a.data());
    av.push_back(nullptr);
    std::vector<std::string> env = childEnvironment(cfg.bundled);
    std::vector<char*> ev;
    for (std::string& e : env) ev.push_back(e.data());
    ev.push_back(nullptr);

    pid_t pid = -1;
    const int rc = posix_spawn(&pid, cfg.python.c_str(), &fa, nullptr, av.data(), ev.data());
    posix_spawn_file_actions_destroy(&fa);
    close(in[0]);
    close(out[1]);
    if (logFd >= 0) close(logFd);
    if (rc != 0) {
        error = "could not start " + cfg.python + ": " + std::strerror(rc);
        close(in[1]);
        close(out[0]);
        return false;
    }
    signal(SIGPIPE, SIG_IGN); // a write after the child died must fail, not kill tsv
    pid_ = pid;
    stdinFd_ = in[1];
    stdoutFd_ = out[0];
    reader_ = std::thread([this] { readLoop(); });
    return true;
#endif
}

#ifndef _WIN32
bool ZeitProcess::reap(bool wait) const {
    std::lock_guard<std::mutex> lk(waitM_);
    if (reaped_) return true;
    if (pid_ <= 0) return false;
    int st = 0;
    const pid_t r = waitpid(pid_, &st, wait ? 0 : WNOHANG);
    if (r == pid_) {
        reaped_ = true;
        status_ = st;
    }
    return reaped_;
}
#endif

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
#else
    std::string buf;
    char chunk[65536];
    for (;;) {
        const ssize_t n = read(stdoutFd_, chunk, sizeof(chunk));
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        buf.append(chunk, size_t(n));
        size_t pos;
        while ((pos = buf.find('\n')) != std::string::npos) {
            std::string line = buf.substr(0, pos);
            buf.erase(0, pos + 1);
            if (!line.empty() && line.back() == '\r') line.pop_back();
            if (!line.empty() && onLine) onLine(line);
        }
    }
    for (int i = 0; i < 500 && !reap(false); ++i) usleep(10000); // the process closes stdout as it exits
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
    if (stdinFd_ < 0) return false;
    const std::string data = line + "\n";
    size_t done = 0;
    while (done < data.size()) {
        const ssize_t n = write(stdinFd_, data.data() + done, data.size() - done);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return false;
        done += size_t(n);
    }
    return true;
#endif
}

void ZeitProcess::terminate() {
#ifdef _WIN32
    if (process_) TerminateProcess(process_, 1);
#else
    if (pid_ > 0 && !reap(false)) kill(pid_, SIGKILL); // like TerminateProcess: no cleanup in the bridge
#endif
}

bool ZeitProcess::running() const {
#ifdef _WIN32
    return process_ && WaitForSingleObject(process_, 0) == WAIT_TIMEOUT;
#else
    return pid_ > 0 && !reap(false);
#endif
}

int ZeitProcess::exitCode() const {
#ifdef _WIN32
    DWORD code = 0;
    if (process_ && GetExitCodeProcess(process_, &code)) return int(code);
#else
    if (reap(false)) {
        std::lock_guard<std::mutex> lk(waitM_);
        if (WIFEXITED(status_)) return WEXITSTATUS(status_);
        if (WIFSIGNALED(status_)) return 128 + WTERMSIG(status_);
    }
#endif
    return -1;
}

std::string bundledPython(const std::string& runtimeDir) {
    const fs::path py = fs::u8path(runtimeDir) / "python";
#ifdef _WIN32
    return (py / "python.exe").u8string();
#else
    return (py / "bin" / "python3").u8string();
#endif
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
            tool.bands = t["requires"].value("bands", std::vector<std::string>());
            tool.optionalBands = t["requires"].value("optional_bands", std::vector<std::string>());
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
                              const std::vector<float>& values, const json& extra) {
    if (state_ != State::Ready || !proc_) return 0;
    const uint64_t id = nextId_++;
    json vals = json::array();
    for (float v : values) vals.push_back(std::isfinite(v) ? json(double(v)) : json());
    json p = {{"tool", toolId}, {"params", params}, {"years", years}, {"values", vals}};
    if (extra.is_object()) p.update(extra);
    const json req = {{"id", id}, {"method", "run_pixel"}, {"params", p}};
    return proc_->writeLine(req.dump()) ? id : 0;
}

double estimateJobSeconds(const json& e, int width, int height) {
    const double perPx = e.value("sec_per_px", -1.0), perChunk = e.value("sec_per_chunk", 0.0);
    if (perPx < 0 || width <= 0 || height <= 0) return -1;
    const double cells = e.value("chunk_cells", 4000000.0);
    const int rows = std::clamp(int(cells / width), 8, 512); // as the bridge's rows_per_chunk
    const int chunks = (height + rows - 1) / rows;
    return chunks * perChunk + double(width) * height * perPx;
}

uint64_t ZeitClient::call(const std::string& method, const json& params) {
    if (state_ != State::Ready || !proc_) return 0;
    const uint64_t id = nextId_++;
    const json req = {{"id", id}, {"method", method}, {"params", params}};
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

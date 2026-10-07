#include "file_browser.hpp"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <filesystem>

#include <imgui.h>

#include "cube.hpp"
#include "gl.hpp" // glfwPostEmptyEvent: wake the UI when a listing finishes
#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

std::string lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(), [](unsigned char c) { return char(std::tolower(c)); });
    return s;
}

std::string humanSize(uint64_t b) {
    char buf[32];
    if (b >= (1ull << 30)) std::snprintf(buf, sizeof(buf), "%.1f GB", b / double(1ull << 30));
    else if (b >= (1ull << 20)) std::snprintf(buf, sizeof(buf), "%.1f MB", b / double(1ull << 20));
    else std::snprintf(buf, sizeof(buf), "%.0f KB", b / 1024.0);
    return buf;
}


} // namespace

std::unique_ptr<FileBrowser::Node> FileBrowser::makeNode(const Entry& e) {
    auto n = std::make_unique<Node>();
    n->e = e;
    return n;
}

FileBrowser::FileBrowser() {
    for (const std::string& r : platform::rootFolders()) {
        Entry e;
        e.path = r;
        e.name = r;
        e.dir = true;
        roots_.push_back(makeNode(e));
    }
}

void FileBrowser::addRecent(const std::string& path) {
    std::error_code ec;
    fs::path p = fs::u8path(path);
    if (!fs::is_directory(p, ec)) p = p.parent_path();
    const std::string dir = p.u8string();
    recent_.erase(std::remove(recent_.begin(), recent_.end(), dir), recent_.end());
    recent_.insert(recent_.begin(), dir);
    if (recent_.size() > 8) recent_.resize(8);
}

void FileBrowser::reveal(const std::string& folder) { revealPath_ = lower(fs::u8path(folder).u8string()); }

bool FileBrowser::visible(const Entry& e) const {
    if (filter_[0] && lower(e.name).find(lower(filter_)) == std::string::npos && !e.dir) return false;
    return e.dir || showAll_ || isRasterPath(e.path);
}

void FileBrowser::startListing(Node& n) {
    const std::string path = n.e.path;
    n.pending = std::async(std::launch::async, [path] {
        std::vector<Entry> out;
        std::error_code ec;
        for (auto it = fs::directory_iterator(fs::u8path(path), fs::directory_options::skip_permission_denied, ec);
             !ec && it != fs::directory_iterator(); it.increment(ec)) {
            Entry e;
            e.path = it->path().u8string();
            e.name = it->path().filename().u8string();
            std::error_code ec2;
            e.dir = it->is_directory(ec2);
            if (!e.dir) e.size = it->file_size(ec2);
            if (!e.name.empty() && e.name[0] == '$') continue; // $Recycle.Bin and friends
            out.push_back(std::move(e));
        }
        std::sort(out.begin(), out.end(), [](const Entry& a, const Entry& b) {
            if (a.dir != b.dir) return a.dir;
            return lower(a.name) < lower(b.name);
        });
        glfwPostEmptyEvent();
        return out;
    });
}

void FileBrowser::drawNode(Node& n, Action& act) {
    ImGui::PushID(n.e.path.c_str());
    if (n.e.dir) {
        // Auto-expand the folders on the way to the revealed path.
        const std::string lp = lower(n.e.path);
        if (!revealPath_.empty() && revealPath_.rfind(lp, 0) == 0) ImGui::SetNextItemOpen(true);
        const bool open = ImGui::TreeNodeEx(n.e.name.c_str(), ImGuiTreeNodeFlags_OpenOnArrow |
                                                                  ImGuiTreeNodeFlags_OpenOnDoubleClick |
                                                                  ImGuiTreeNodeFlags_SpanAvailWidth);
        if (ImGui::BeginPopupContextItem()) {
            if (ImGui::MenuItem("Open folder as series")) act = {Action::Open, {n.e.path}};
            if (ImGui::MenuItem("Add folder as layer")) act = {Action::AddLayer, {n.e.path}};
            if (ImGui::MenuItem("Show in Explorer")) platform::openInExplorer(n.e.path);
            ImGui::EndPopup();
        }
        if (open) {
            if (!n.listed && !n.pending.valid()) startListing(n);
            if (n.pending.valid()) {
                if (n.pending.wait_for(std::chrono::seconds(0)) == std::future_status::ready) {
                    for (Entry& e : n.pending.get()) n.children.push_back(makeNode(e));
                    n.listed = true;
                } else {
                    ImGui::TextDisabled("listing...");
                }
            }
            int rasters = 0;
            for (auto& c : n.children) {
                if (!visible(c->e)) continue;
                if (!c->e.dir) ++rasters;
                drawNode(*c, act);
            }
            if (n.listed && n.children.empty()) ImGui::TextDisabled("(empty)");
            if (rasters > 1) {
                if (ImGui::SmallButton("Open these files as a series")) {
                    act.kind = Action::Open;
                    act.paths.clear();
                    for (auto& c : n.children)
                        if (!c->e.dir && visible(c->e)) act.paths.push_back(c->e.path);
                }
            }
            ImGui::TreePop();
        } else if (!revealPath_.empty() && lp == revealPath_) {
            revealPath_.clear();
        }
    } else {
        const bool sel = selected_.count(n.e.path) > 0;
        const std::string label = n.e.name + "##f";
        if (ImGui::Selectable(label.c_str(), sel, ImGuiSelectableFlags_AllowDoubleClick)) {
            if (ImGui::GetIO().KeyCtrl) {
                if (sel) selected_.erase(n.e.path);
                else selected_.insert(n.e.path);
            } else {
                selected_ = {n.e.path};
            }
            if (ImGui::IsMouseDoubleClicked(ImGuiMouseButton_Left)) act = {Action::Open, {n.e.path}};
        }
        ImGui::SetItemTooltip("%s\n%s\nDouble click: open as series. Ctrl+click: select several.",
                              n.e.path.c_str(), humanSize(n.e.size).c_str());
        if (ImGui::BeginPopupContextItem()) {
            std::vector<std::string> paths(selected_.begin(), selected_.end());
            if (!selected_.count(n.e.path)) paths = {n.e.path};
            if (ImGui::MenuItem(paths.size() > 1 ? "Open selected files as a series" : "Open as series"))
                act = {Action::Open, paths};
            if (ImGui::MenuItem("Add as layer")) act = {Action::AddLayer, paths};
            if (ImGui::MenuItem("Show in Explorer")) platform::openInExplorer(fs::u8path(n.e.path).parent_path().u8string());
            ImGui::EndPopup();
        }
        ImGui::SameLine(ImGui::GetContentRegionMax().x - 70);
        ImGui::TextDisabled("%s", humanSize(n.e.size).c_str());
    }
    ImGui::PopID();
}

FileBrowser::Action FileBrowser::draw() {
    Action act;
    ImGui::Checkbox("All files", &showAll_);
    ImGui::SetItemTooltip("Off: only rasters (.tif, .tiff, .vrt, .nc, .img, .jp2, ...)");
    ImGui::SameLine();
    ImGui::SetNextItemWidth(-1);
    ImGui::InputTextWithHint("##filter", "filter by name", filter_, sizeof(filter_));
    if (!selected_.empty()) {
        if (ImGui::Button(selected_.size() > 1 ? "Open selected as series" : "Open")) {
            act = {Action::Open, std::vector<std::string>(selected_.begin(), selected_.end())};
        }
        ImGui::SameLine();
        if (ImGui::Button("Add as layer")) {
            act = {Action::AddLayer, std::vector<std::string>(selected_.begin(), selected_.end())};
        }
        ImGui::SameLine();
        ImGui::TextDisabled("%d selected", int(selected_.size()));
    }
    ImGui::Separator();
    ImGui::BeginChild("tree");
    if (!recent_.empty() && ImGui::TreeNodeEx("Recent", ImGuiTreeNodeFlags_DefaultOpen)) {
        for (const std::string& r : recent_) {
            ImGui::PushID(r.c_str());
            if (ImGui::Selectable(r.c_str(), false, ImGuiSelectableFlags_AllowDoubleClick) &&
                ImGui::IsMouseDoubleClicked(ImGuiMouseButton_Left))
                act = {Action::Open, {r}};
            ImGui::SetItemTooltip("Double click: open this folder as a series.\nRight click: more options.");
            if (ImGui::BeginPopupContextItem()) {
                if (ImGui::MenuItem("Open folder as series")) act = {Action::Open, {r}};
                if (ImGui::MenuItem("Add folder as layer")) act = {Action::AddLayer, {r}};
                if (ImGui::MenuItem("Reveal in tree")) reveal(r);
                ImGui::EndPopup();
            }
            ImGui::PopID();
        }
        ImGui::TreePop();
    }
    for (auto& r : roots_) drawNode(*r, act);
    ImGui::EndChild();
    return act;
}

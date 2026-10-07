#pragma once

#include <cstdint>
#include <future>
#include <memory>
#include <set>
#include <string>
#include <vector>

// Files panel: a lazily listed folder tree (rasters only by default) to open
// series quickly without the Windows dialog. Folders are listed in the
// background the first time they are expanded, so slow disks or network shares
// never freeze the UI.
class FileBrowser {
public:
    struct Action {
        enum Kind { None, Open, AddLayer } kind = None;
        std::vector<std::string> paths;
    };

    FileBrowser();
    // Draws the panel contents (call inside an ImGui window).
    Action draw();
    // Remembers the folder of a series that was opened (shown under "Recent").
    void addRecent(const std::string& path);
    // Expands the tree down to this folder (e.g. the folder of the open series).
    void reveal(const std::string& folder);

private:
    struct Entry {
        std::string path, name;
        bool dir = false;
        uint64_t size = 0;
    };
    struct Node {
        Entry e;
        bool listed = false;
        std::string error;
        std::vector<std::unique_ptr<Node>> children;
        std::future<std::vector<Entry>> pending;
    };

    void drawNode(Node& n, Action& act);
    void startListing(Node& n);
    bool visible(const Entry& e) const;
    static std::unique_ptr<Node> makeNode(const Entry& e);

    std::vector<std::unique_ptr<Node>> roots_;
    std::vector<std::string> recent_;
    std::set<std::string> selected_;
    std::string revealPath_;
    bool showAll_ = false;
    char filter_[128] = "";
};

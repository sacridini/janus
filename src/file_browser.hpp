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
    // Expands the tree down to this folder, or to the folder of this file, and
    // scrolls to it (e.g. a series just opened). The folder is listed again, so
    // files written since it was first listed show up.
    void reveal(const std::string& path);
    bool revealing() const { return !revealDir_.empty(); }
    const std::string& revealed() const { return revealed_; } // the last item scrolled to (lower case)

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

    // `reveal`: the node is in the root the revealed path is under.
    void drawNode(Node& n, Action& act, bool reveal);
    void startListing(Node& n);
    bool visible(const Entry& e) const;
    static std::unique_ptr<Node> makeNode(const Entry& e);

    std::vector<std::unique_ptr<Node>> roots_;
    std::vector<std::string> recent_;
    std::set<std::string> selected_;
    // Being revealed (lower case, see reveal()): the folder to open and the
    // item to scroll to (that folder, or a file in it).
    std::string revealDir_, revealItem_;
    bool revealRelist_ = false;
    std::string revealed_;
    bool showAll_ = false;
    char filter_[128] = "";
};

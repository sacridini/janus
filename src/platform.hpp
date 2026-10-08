#pragma once

#include <ctime>
#include <string>
#include <vector>

// Operating-system specific functions. All paths are UTF-8.
namespace platform {

void init();                                  // COM etc. (call at the start of main)
std::vector<std::string> commandLineArgs(int argc, char** argv); // without argv[0]
void attachParentConsole();                   // for --help from a GUI app (Windows)

std::string exeDir();
// Bundled data (share/, runtime/): exeDir(), or Contents/Resources inside a macOS .app.
std::string resourceDir();
std::string appDataDir();                     // e.g. %LOCALAPPDATA%\Janus (created if needed)
std::string cacheDir();                       // regenerable data: appDataDir()/cache; macOS: ~/Library/Caches/Janus

std::vector<std::string> openFilesDialog();   // empty if cancelled
std::string openFolderDialog();               // empty if cancelled
// Asks where to save a file: `ext` without the dot (e.g. "png"), added when the
// name typed has another one; `folder` = where the dialog starts (may be empty).
// The OS dialog confirms overwriting. Empty if cancelled.
std::string saveFileDialog(const std::string& title, const std::string& defaultName, const std::string& filterName,
                           const std::string& ext, const std::string& folder);
void openInExplorer(const std::string& path); // folder or file, with the default app
std::string getEnv(const char* name);         // UTF-8, empty if unset
std::vector<std::string> rootFolders();      // drives (C:\, D:\...) and the user's home

// True if the file lives on a disk with a seek penalty (spinning HDD). Used to
// limit parallel reads, which make an HDD's head jump back and forth.
bool isOnRotationalDisk(const std::string& path);

std::tm localTime(std::time_t t);           // thread-safe localtime

// Trackpad gestures (call once per frame). GLFW reports neither pinches nor
// whether a scroll came from a trackpad: on macOS (platform_mac.mm) pinch is the
// zoom factor since the previous call (1 = none) and touchScroll is true when
// the latest scroll came from a touch surface (trackpad, Magic Mouse: the events
// carry gesture phases), which pans the map instead of zooming it; a mouse wheel,
// even one with smooth (precise) deltas, zooms. Elsewhere: no pinch, a scroll is
// a mouse wheel.
struct Gestures {
    double pinch = 1.0;
    bool touchScroll = false;
};
Gestures takeGestures();

// Files and folders the OS asked us to open while running or at launch (macOS:
// Finder's "Open With", a double click, a drop on the Dock icon). Elsewhere
// they come on the command line: always empty.
std::vector<std::string> takeOpenRequests();

} // namespace platform

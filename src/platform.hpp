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
std::string appDataDir();                     // e.g. %LOCALAPPDATA%\tsv (created if needed)

std::vector<std::string> openFilesDialog();   // empty if cancelled
std::string openFolderDialog();               // empty if cancelled
void openInExplorer(const std::string& path); // folder or file, with the default app
std::string getEnv(const char* name);         // UTF-8, empty if unset
std::vector<std::string> rootFolders();      // drives (C:\, D:\...) and the user's home

// True if the file lives on a disk with a seek penalty (spinning HDD). Used to
// limit parallel reads, which make an HDD's head jump back and forth.
bool isOnRotationalDisk(const std::string& path);

std::tm localTime(std::time_t t);           // thread-safe localtime

} // namespace platform

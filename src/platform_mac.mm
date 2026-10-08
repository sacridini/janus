// macOS-only parts of platform.hpp that need Cocoa.

#include "platform.hpp"

#import <AppKit/AppKit.h>
#import <objc/runtime.h>

namespace {

double g_pinch = 1.0;
bool g_touchScroll = false;
std::vector<std::string> g_openRequests; // main thread only

// -[NSApplicationDelegate application:openURLs:]: how the Finder hands over
// documents (Open With, double click, a drop on the Dock icon).
void openURLs(id, SEL, NSApplication*, NSArray<NSURL*>* urls) {
    for (NSURL* u in urls)
        if (u.isFileURL) g_openRequests.emplace_back(u.path.fileSystemRepresentation);
}

// A local monitor sees every event of the application before it is dispatched
// (GLFW still gets the scroll events: the other panels keep scrolling).
void installMonitor() {
    static bool installed = false;
    if (installed || NSApp == nil) return;
    installed = true;
    [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskMagnify | NSEventMaskScrollWheel
                                          handler:^NSEvent*(NSEvent* e) {
                                              if (e.type == NSEventTypeMagnify)
                                                  g_pinch *= 1.0 + e.magnification;
                                              else // a touch surface reports the gesture's phases (and its
                                                   // inertia); a wheel never does, smooth or not
                                                  g_touchScroll = e.phase != NSEventPhaseNone ||
                                                                  e.momentumPhase != NSEventPhaseNone;
                                              return e;
                                          }];
}

} // namespace

void platform::init() {
    // GLFW's application delegate does not answer application:openURLs:, so
    // documents given at launch (delivered inside glfwInit) would be lost. Add
    // it to the class before glfwInit creates the delegate.
    if (Class c = NSClassFromString(@"GLFWApplicationDelegate"))
        class_addMethod(c, @selector(application:openURLs:), (IMP)openURLs, "v@:@@");
}

std::vector<std::string> platform::takeOpenRequests() {
    std::vector<std::string> out;
    out.swap(g_openRequests);
    return out;
}

platform::Gestures platform::takeGestures() {
    installMonitor(); // NSApp exists once GLFW is initialized
    Gestures g;
    g.pinch = g_pinch;
    g.touchScroll = g_touchScroll;
    g_pinch = 1.0;
    return g;
}

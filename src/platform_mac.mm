// macOS-only parts of platform.hpp that need Cocoa.

#include "platform.hpp"

#import <AppKit/AppKit.h>

namespace {

double g_pinch = 1.0;
bool g_preciseScroll = false;

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
                                              else
                                                  g_preciseScroll = e.hasPreciseScrollingDeltas;
                                              return e;
                                          }];
}

} // namespace

platform::Gestures platform::takeGestures() {
    installMonitor(); // NSApp exists once GLFW is initialized
    Gestures g;
    g.pinch = g_pinch;
    g.preciseScroll = g_preciseScroll;
    g_pinch = 1.0;
    return g;
}

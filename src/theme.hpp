#pragma once

#include <imgui.h>

// Interface themes (Settings): ImGui's and ImPlot's colours, and the colours
// Janus draws itself in the interface (status text, chart series), kept
// legible on each theme. The map is data: its background, colour bars and
// marks stay the same on every theme (and in exported figures).
namespace theme {

enum Id { Dark = 0, Light, Classic, Janus, Studio, Graphite, Count };
const char* name(int id);
// Sets ImGui's and ImPlot's colours (and Janus' fixed style: square, opaque
// windows, so detached panels look like OS windows).
void apply(int id);
int current();

// Status text on the window background.
ImVec4 accent();       // active layer, tool in use
ImVec4 warning();
ImVec4 error();
ImVec4 cursorSeries(); // the cursor's series on the chart (white on dark themes)
// Where no panel is (behind the dock space).
void clearColor(float rgba[4]);

// WCAG contrast ratio of two colours (alpha ignored), 1 to 21.
float contrast(const ImVec4& a, const ImVec4& b);
// `c` darkened (on a light background) or lightened (on a dark one), keeping
// its hue, as little as needed to reach `ratio` against `bg`.
ImVec4 legible(const ImVec4& c, const ImVec4& bg, float ratio);
// ImGui colour `idx` as seen over the window background.
ImVec4 background(ImGuiCol idx = ImGuiCol_WindowBg);
// The charts' plot area (ImPlot's, over the window).
ImVec4 plotBackground();
// A data colour (pin, ROI, model...) on the charts: 3:1, as asked of graphics.
ImVec4 onPlot(const ImVec4& c);
// A data colour used as text over `bg` (e.g. a pin's name in a table header): 4.5:1.
ImVec4 onWindow(const ImVec4& c, ImGuiCol bg = ImGuiCol_WindowBg);

} // namespace theme

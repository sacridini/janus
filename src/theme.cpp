#include "theme.hpp"

#include <algorithm>
#include <cmath>

#include <implot.h>

namespace theme {

namespace {

int g_theme = Dark;

ImVec4 rgb(int hex, float a = 1.0f) {
    return ImVec4(((hex >> 16) & 0xFF) / 255.f, ((hex >> 8) & 0xFF) / 255.f, (hex & 0xFF) / 255.f, a);
}

ImVec4 mix(const ImVec4& a, const ImVec4& b, float t) {
    return ImVec4(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t, a.w + (b.w - a.w) * t);
}

// `top` drawn over the opaque `under`.
ImVec4 over(const ImVec4& top, const ImVec4& under) {
    const float a = std::clamp(top.w, 0.0f, 1.0f);
    return ImVec4(top.x * a + under.x * (1 - a), top.y * a + under.y * (1 - a), top.z * a + under.z * (1 - a), 1);
}

float luminance(const ImVec4& c) {
    auto lin = [](float v) { return v <= 0.04045f ? v / 12.92f : std::pow((v + 0.055f) / 1.055f, 2.4f); };
    return 0.2126f * lin(c.x) + 0.7152f * lin(c.y) + 0.0722f * lin(c.z);
}

// Janus' visual identity (branding/README.md): ImGui blue #4296FA, window
// #0F0F0F, panel #1C2A3C, date marker orange #F29A38 only as an accent.
void janusColors(ImVec4* c) {
    ImGui::StyleColorsDark();
    const ImVec4 blue = rgb(0x4296FA), panel = rgb(0x1C2A3C), orange = rgb(0xF29A38);
    c[ImGuiCol_Text] = ImVec4(0.92f, 0.94f, 0.97f, 1);
    c[ImGuiCol_TextDisabled] = ImVec4(0.53f, 0.59f, 0.67f, 1);
    c[ImGuiCol_WindowBg] = rgb(0x0F0F0F);
    c[ImGuiCol_PopupBg] = ImVec4(0.07f, 0.09f, 0.13f, 0.98f);
    c[ImGuiCol_Border] = ImVec4(0.20f, 0.28f, 0.38f, 0.60f);
    c[ImGuiCol_FrameBg] = panel;
    c[ImGuiCol_FrameBgHovered] = mix(panel, blue, 0.30f);
    c[ImGuiCol_FrameBgActive] = mix(panel, blue, 0.45f);
    c[ImGuiCol_TitleBg] = ImVec4(0.06f, 0.08f, 0.11f, 1);
    c[ImGuiCol_TitleBgActive] = panel;
    c[ImGuiCol_MenuBarBg] = ImVec4(0.08f, 0.11f, 0.15f, 1);
    c[ImGuiCol_ScrollbarBg] = ImVec4(0.04f, 0.05f, 0.07f, 0.60f);
    c[ImGuiCol_ScrollbarGrab] = ImVec4(0.19f, 0.26f, 0.36f, 1);
    c[ImGuiCol_ScrollbarGrabHovered] = ImVec4(0.25f, 0.34f, 0.47f, 1);
    c[ImGuiCol_ScrollbarGrabActive] = ImVec4(0.31f, 0.42f, 0.58f, 1);
    c[ImGuiCol_Separator] = c[ImGuiCol_Border];
    c[ImGuiCol_Tab] = mix(c[ImGuiCol_Header], panel, 0.80f);
    c[ImGuiCol_TabSelected] = mix(c[ImGuiCol_HeaderActive], panel, 0.60f);
    c[ImGuiCol_TabDimmed] = mix(c[ImGuiCol_Tab], c[ImGuiCol_TitleBg], 0.80f);
    c[ImGuiCol_TabDimmedSelected] = mix(c[ImGuiCol_TabSelected], c[ImGuiCol_TitleBg], 0.40f);
    c[ImGuiCol_DockingEmptyBg] = ImVec4(0.04f, 0.05f, 0.07f, 1);
    c[ImGuiCol_PlotHistogram] = orange; // progress bars
    c[ImGuiCol_PlotHistogramHovered] = mix(orange, ImVec4(1, 1, 1, 1), 0.25f);
    c[ImGuiCol_TableHeaderBg] = panel;
    c[ImGuiCol_TableBorderStrong] = ImVec4(0.20f, 0.28f, 0.38f, 1);
    c[ImGuiCol_TableBorderLight] = ImVec4(0.14f, 0.20f, 0.28f, 1);
    c[ImGuiCol_TextLink] = orange;
    c[ImGuiCol_DragDropTarget] = orange;
}

struct Status {
    ImVec4 accent, warning, error, cursor, clear;
};

const Status& status() {
    static const Status dark{{0.55f, 0.80f, 1.0f, 1}, {1.0f, 0.7f, 0.4f, 1}, {1.0f, 0.55f, 0.45f, 1},
                             {0.95f, 0.95f, 0.95f, 1}, {0.08f, 0.08f, 0.09f, 1}};
    static const Status light{{0.05f, 0.33f, 0.68f, 1}, {0.60f, 0.32f, 0.0f, 1}, {0.75f, 0.12f, 0.10f, 1},
                              {0.10f, 0.10f, 0.12f, 1}, {0.80f, 0.80f, 0.82f, 1}};
    static const Status janus{{0.45f, 0.70f, 1.0f, 1}, rgb(0xF29A38), {1.0f, 0.52f, 0.45f, 1},
                              {0.95f, 0.95f, 0.95f, 1}, {0.04f, 0.05f, 0.07f, 1}};
    return g_theme == Light ? light : g_theme == Janus ? janus : dark;
}

} // namespace

const char* name(int id) {
    static const char* names[Count] = {"Dark", "Light", "Classic", "Janus"};
    return names[std::clamp(id, 0, Count - 1)];
}

int current() { return g_theme; }

void apply(int id) {
    g_theme = std::clamp(id, 0, Count - 1);
    ImGuiStyle& style = ImGui::GetStyle();
    ImVec4* c = style.Colors;
    switch (g_theme) {
    case Light:
        ImGui::StyleColorsLight();
        ImPlot::StyleColorsLight();
        c[ImGuiCol_TextDisabled] = ImVec4(0.42f, 0.42f, 0.42f, 1); // 0.60 gives 2.5:1 on the window
        break;
    case Classic:
        ImGui::StyleColorsClassic();
        ImPlot::StyleColorsClassic();
        break;
    case Janus:
        janusColors(c);
        ImPlot::StyleColorsAuto(); // from the ImGui colours above
        ImPlot::GetStyle().Colors[ImPlotCol_Selection] = rgb(0xF29A38);
        break;
    default: // today's look: ImPlot follows ImGui
        ImGui::StyleColorsDark();
        ImPlot::StyleColorsAuto();
        break;
    }
    // Platform windows look like regular OS windows: no rounding, opaque.
    style.WindowRounding = 0.0f;
    c[ImGuiCol_WindowBg].w = 1.0f;
}

ImVec4 accent() { return status().accent; }
ImVec4 warning() { return status().warning; }
ImVec4 error() { return status().error; }
ImVec4 cursorSeries() { return status().cursor; }

void clearColor(float out[4]) {
    const ImVec4& c = status().clear;
    out[0] = c.x;
    out[1] = c.y;
    out[2] = c.z;
    out[3] = 1.0f;
}

float contrast(const ImVec4& a, const ImVec4& b) {
    const float la = luminance(a), lb = luminance(b);
    return (std::max(la, lb) + 0.05f) / (std::min(la, lb) + 0.05f);
}

ImVec4 legible(const ImVec4& c, const ImVec4& bg, float ratio) {
    if (contrast(c, bg) >= ratio) return c;
    // Towards black on a light background, towards white on a dark one: the
    // smallest step (bisection) that reaches the ratio.
    const ImVec4 target = luminance(bg) > 0.18f ? ImVec4(0, 0, 0, c.w) : ImVec4(1, 1, 1, c.w);
    float lo = 0, hi = 1;
    for (int i = 0; i < 16; ++i) {
        const float t = (lo + hi) * 0.5f;
        (contrast(mix(c, target, t), bg) >= ratio ? hi : lo) = t;
    }
    return mix(c, target, hi);
}

ImVec4 background(ImGuiCol idx) {
    const ImVec4 window = over(ImGui::GetStyleColorVec4(ImGuiCol_WindowBg), ImVec4(0, 0, 0, 1));
    return idx == ImGuiCol_WindowBg ? window : over(ImGui::GetStyleColorVec4(idx), window);
}

ImVec4 plotBackground() {
    const ImVec4 c = ImPlot::GetStyle().Colors[ImPlotCol_PlotBg];
    const bool automatic = c.w == -1; // IMPLOT_AUTO_COL: the window's
    return automatic ? background() : over(c, background());
}

ImVec4 onPlot(const ImVec4& c) { return legible(c, plotBackground(), 3.0f); }

ImVec4 onWindow(const ImVec4& c, ImGuiCol bg) { return legible(c, background(bg), 4.5f); }

} // namespace theme

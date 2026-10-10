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

// Grey studio looks, after the audio programs: flat controls with a thin
// border, one warm accent (active toggles, sliders, progress).
// Studio: Ableton Live's mid grey, dark text, the charts in a dark display.
void studioColors(ImVec4* c) {
    ImGui::StyleColorsLight();
    const ImVec4 orange = rgb(0xFF9A1F), win = rgb(0xB0B0B0);
    c[ImGuiCol_Text] = rgb(0x101010);
    c[ImGuiCol_TextDisabled] = rgb(0x3C3C3C);
    c[ImGuiCol_WindowBg] = win;
    c[ImGuiCol_ChildBg] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_PopupBg] = rgb(0xBEBEBE);
    c[ImGuiCol_Border] = rgb(0x6E6E6E);
    c[ImGuiCol_BorderShadow] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_FrameBg] = rgb(0xC6C6C6);
    c[ImGuiCol_FrameBgHovered] = rgb(0xD2D2D2);
    c[ImGuiCol_FrameBgActive] = rgb(0xDCDCDC);
    c[ImGuiCol_TitleBg] = rgb(0x8C8C8C);
    c[ImGuiCol_TitleBgActive] = rgb(0x9A9A9A);
    c[ImGuiCol_TitleBgCollapsed] = rgb(0x8C8C8C);
    c[ImGuiCol_MenuBarBg] = rgb(0x9E9E9E);
    c[ImGuiCol_ScrollbarBg] = rgb(0xA2A2A2);
    c[ImGuiCol_ScrollbarGrab] = rgb(0x7E7E7E);
    c[ImGuiCol_ScrollbarGrabHovered] = rgb(0x6C6C6C);
    c[ImGuiCol_ScrollbarGrabActive] = rgb(0x5A5A5A);
    c[ImGuiCol_CheckMark] = rgb(0x1A1A1A);
    c[ImGuiCol_SliderGrab] = rgb(0x5E5E5E);
    c[ImGuiCol_SliderGrabActive] = orange;
    c[ImGuiCol_Button] = rgb(0xA0A0A0);
    c[ImGuiCol_ButtonHovered] = rgb(0xBDBDBD);
    c[ImGuiCol_ButtonActive] = orange;
    c[ImGuiCol_Header] = rgb(0x9C9C9C);
    c[ImGuiCol_HeaderHovered] = rgb(0xC0C0C0);
    c[ImGuiCol_HeaderActive] = mix(orange, win, 0.35f);
    c[ImGuiCol_Separator] = rgb(0x7E7E7E);
    c[ImGuiCol_SeparatorHovered] = orange;
    c[ImGuiCol_SeparatorActive] = orange;
    c[ImGuiCol_ResizeGrip] = ImVec4(0, 0, 0, 0.15f);
    c[ImGuiCol_ResizeGripHovered] = orange;
    c[ImGuiCol_ResizeGripActive] = orange;
    c[ImGuiCol_Tab] = rgb(0x969696);
    c[ImGuiCol_TabHovered] = rgb(0xC0C0C0);
    c[ImGuiCol_TabSelected] = win;
    c[ImGuiCol_TabSelectedOverline] = orange;
    c[ImGuiCol_TabDimmed] = rgb(0x8C8C8C);
    c[ImGuiCol_TabDimmedSelected] = rgb(0xA4A4A4);
    c[ImGuiCol_TabDimmedSelectedOverline] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_DockingPreview] = mix(orange, win, 0.3f);
    c[ImGuiCol_DockingEmptyBg] = rgb(0x8A8A8A);
    c[ImGuiCol_PlotHistogram] = orange; // progress bars
    c[ImGuiCol_PlotHistogramHovered] = mix(orange, ImVec4(1, 1, 1, 1), 0.25f);
    c[ImGuiCol_TableHeaderBg] = rgb(0x9C9C9C);
    c[ImGuiCol_TableBorderStrong] = rgb(0x6E6E6E);
    c[ImGuiCol_TableBorderLight] = rgb(0x8E8E8E);
    c[ImGuiCol_TableRowBgAlt] = ImVec4(1, 1, 1, 0.06f);
    c[ImGuiCol_TextSelectedBg] = mix(orange, win, 0.45f);
    c[ImGuiCol_TextLink] = rgb(0x0A3D73);
    c[ImGuiCol_DragDropTarget] = orange;
    c[ImGuiCol_ModalWindowDimBg] = ImVec4(0.2f, 0.2f, 0.2f, 0.35f);
}

// The charts of Studio: a dark display (as Live's clip and device displays),
// so the data colours keep their brightness.
void studioPlot() {
    ImPlot::StyleColorsAuto();
    ImVec4* p = ImPlot::GetStyle().Colors;
    p[ImPlotCol_PlotBg] = rgb(0x2A2A2A);
    p[ImPlotCol_PlotBorder] = rgb(0x6E6E6E);
    p[ImPlotCol_LegendBg] = ImVec4(0.16f, 0.16f, 0.16f, 0.92f);
    p[ImPlotCol_LegendBorder] = rgb(0x5A5A5A);
    p[ImPlotCol_LegendText] = rgb(0xE0E0E0);
    p[ImPlotCol_InlayText] = rgb(0xE0E0E0);
    p[ImPlotCol_AxisGrid] = ImVec4(1, 1, 1, 0.13f);
    p[ImPlotCol_Selection] = rgb(0xFF9A1F);
    p[ImPlotCol_Crosshairs] = ImVec4(1, 1, 1, 0.5f);
}

// Graphite: Max's dark grey, darker fields, light text, a yellow accent.
void graphiteColors(ImVec4* c) {
    ImGui::StyleColorsDark();
    const ImVec4 yellow = rgb(0xF5B22E), win = rgb(0x333333);
    c[ImGuiCol_Text] = rgb(0xDCDCDC);
    c[ImGuiCol_TextDisabled] = rgb(0xA2A2A2);
    c[ImGuiCol_WindowBg] = win;
    c[ImGuiCol_ChildBg] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_PopupBg] = rgb(0x2A2A2A);
    c[ImGuiCol_Border] = rgb(0x4C4C4C);
    c[ImGuiCol_BorderShadow] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_FrameBg] = rgb(0x242424);
    c[ImGuiCol_FrameBgHovered] = rgb(0x2C2C2C);
    c[ImGuiCol_FrameBgActive] = rgb(0x1C1C1C);
    c[ImGuiCol_TitleBg] = rgb(0x262626);
    c[ImGuiCol_TitleBgActive] = rgb(0x2E2E2E);
    c[ImGuiCol_TitleBgCollapsed] = rgb(0x262626);
    c[ImGuiCol_MenuBarBg] = rgb(0x282828);
    c[ImGuiCol_ScrollbarBg] = rgb(0x2B2B2B);
    c[ImGuiCol_ScrollbarGrab] = rgb(0x4E4E4E);
    c[ImGuiCol_ScrollbarGrabHovered] = rgb(0x5C5C5C);
    c[ImGuiCol_ScrollbarGrabActive] = rgb(0x6A6A6A);
    c[ImGuiCol_CheckMark] = yellow;
    c[ImGuiCol_SliderGrab] = rgb(0x8A8A8A);
    c[ImGuiCol_SliderGrabActive] = yellow;
    c[ImGuiCol_Button] = rgb(0x464646);
    c[ImGuiCol_ButtonHovered] = rgb(0x545454);
    c[ImGuiCol_ButtonActive] = mix(yellow, win, 0.45f);
    c[ImGuiCol_Header] = rgb(0x464646);
    c[ImGuiCol_HeaderHovered] = rgb(0x525252);
    c[ImGuiCol_HeaderActive] = mix(yellow, win, 0.60f);
    c[ImGuiCol_Separator] = rgb(0x4C4C4C);
    c[ImGuiCol_SeparatorHovered] = yellow;
    c[ImGuiCol_SeparatorActive] = yellow;
    c[ImGuiCol_ResizeGrip] = ImVec4(1, 1, 1, 0.10f);
    c[ImGuiCol_ResizeGripHovered] = yellow;
    c[ImGuiCol_ResizeGripActive] = yellow;
    c[ImGuiCol_Tab] = rgb(0x2A2A2A);
    c[ImGuiCol_TabHovered] = rgb(0x4A4A4A);
    c[ImGuiCol_TabSelected] = win;
    c[ImGuiCol_TabSelectedOverline] = yellow;
    c[ImGuiCol_TabDimmed] = rgb(0x262626);
    c[ImGuiCol_TabDimmedSelected] = rgb(0x2E2E2E);
    c[ImGuiCol_TabDimmedSelectedOverline] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_DockingPreview] = mix(yellow, win, 0.4f);
    c[ImGuiCol_DockingEmptyBg] = rgb(0x1E1E1E);
    c[ImGuiCol_PlotHistogram] = yellow; // progress bars
    c[ImGuiCol_PlotHistogramHovered] = mix(yellow, ImVec4(1, 1, 1, 1), 0.25f);
    c[ImGuiCol_TableHeaderBg] = rgb(0x2A2A2A);
    c[ImGuiCol_TableBorderStrong] = rgb(0x4C4C4C);
    c[ImGuiCol_TableBorderLight] = rgb(0x3E3E3E);
    c[ImGuiCol_TableRowBgAlt] = ImVec4(1, 1, 1, 0.03f);
    c[ImGuiCol_TextSelectedBg] = mix(yellow, win, 0.65f);
    c[ImGuiCol_TextLink] = yellow;
    c[ImGuiCol_DragDropTarget] = yellow;
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
    // Studio's charts are dark displays: the cursor's series stays white.
    static const Status studio{rgb(0x0A3D73), rgb(0x5C2C00), rgb(0x7A0E0E), {0.95f, 0.95f, 0.95f, 1},
                               rgb(0x8A8A8A)};
    static const Status graphite{rgb(0xF5B22E), {1.0f, 0.62f, 0.38f, 1}, {1.0f, 0.50f, 0.45f, 1},
                                 {0.95f, 0.95f, 0.95f, 1}, rgb(0x1E1E1E)};
    switch (g_theme) {
    case Light: return light;
    case Janus: return janus;
    case Studio: return studio;
    case Graphite: return graphite;
    default: return dark;
    }
}

} // namespace

const char* name(int id) {
    static const char* names[Count] = {"Dark", "Light", "Classic", "Janus", "Studio", "Graphite"};
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
    case Studio:
        studioColors(c);
        studioPlot();
        break;
    case Graphite:
        graphiteColors(c);
        ImPlot::StyleColorsAuto();
        ImPlot::GetStyle().Colors[ImPlotCol_PlotBg] = rgb(0x262626);
        ImPlot::GetStyle().Colors[ImPlotCol_Selection] = rgb(0xF5B22E);
        break;
    default: // today's look: ImPlot follows ImGui
        ImGui::StyleColorsDark();
        ImPlot::StyleColorsAuto();
        break;
    }
    // Platform windows look like regular OS windows: no rounding, opaque.
    style.WindowRounding = 0.0f;
    // The studio themes: flat controls, outlined as in the audio programs.
    const bool studio = g_theme == Studio || g_theme == Graphite;
    style.FrameBorderSize = studio ? 1.0f : 0.0f;
    style.TabBorderSize = studio ? 1.0f : 0.0f;
    style.FrameRounding = g_theme == Graphite ? 2.0f : 0.0f;
    style.GrabRounding = style.FrameRounding;
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

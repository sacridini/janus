// Swipe: the main map left of a draggable divider and, right of it, a
// comparison drawn like a map panel (MapView, renderView) into a GPU target of
// its own: another layer, or the same one at another date or in another mode.
// Also the View menu entries and keys of the swipe and the transect.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>

void App::toggleSwipe() {
    swipe_ = !swipe_ && s_;
    swipeDragging_ = false;
    if (!swipe_) {
        gpu_.releaseMap(kSwipeSlot);
        return;
    }
    swipeView_.id = kSwipeSlot;
    swipeView_.dirty = true;
    swipeView_.detailLevel = -1;
    for (const SeriesLayer& L : layers_) // the previous comparison, if its layer is still open
        if (L.session->info->id == swipeView_.cube) return;
    // Like a new map panel: another layer than the active one if there is one,
    // else the same series at its own date.
    MapView v;
    v.id = kSwipeSlot;
    for (const SeriesLayer& L : layers_)
        if (&L != activeLayer()) {
            v.cube = L.session->info->id;
            break;
        }
    if (!v.cube) {
        v.cube = s_->info->id;
        v.ownDate = true;
        v.t = std::max(0, t_ - 1);
    }
    swipeView_ = v;
}

// The comparison's bar, a row between the time bar and the map.
void App::uiSwipeBar() {
    if (!swipe_ || !s_) return;
    ImGui::PushID("swipe");
    if (ImGui::Button("Swipe off")) toggleSwipe();
    ImGui::SetItemTooltip("Swipe (S): the map left of the divider, this comparison right of it");
    if (swipe_) {
        ImGui::SameLine();
        uiViewBar(swipeView_);
    }
    ImGui::PopID();
}

// Dragging the divider (main map only). True while the divider has the mouse.
bool App::swipeInput(ImVec2 origin, ImVec2 size) {
    if (!swipe_) {
        swipeDragging_ = false;
        return false;
    }
    const ImGuiIO& io = ImGui::GetIO();
    const float x = origin.x + size.x * swipeX_;
    const bool near = ImGui::IsItemHovered() && std::fabs(io.MousePos.x - x) <= 6.0f;
    if (near || swipeDragging_) ImGui::SetMouseCursor(ImGuiMouseCursor_ResizeEW);
    if (ImGui::IsItemActivated() && near && !io.KeyShift && !io.KeyCtrl) swipeDragging_ = true;
    if (!swipeDragging_) return false;
    if (ImGui::IsItemActive()) swipeX_ = std::clamp((io.MousePos.x - origin.x) / size.x, 0.0f, 1.0f);
    else swipeDragging_ = false; // released (this frame's deactivation is swallowed too)
    return true;
}

void App::renderSwipe(ImVec2 size, float pixelScale) {
    renderView(swipeView_, int(size.x), int(size.y), pixelScale, ImVec2(0, 0));
    swipeView_.size = size;
    swipeView_.pixelScale = pixelScale;
    swipeView_.dirty = false;
}

void App::drawSwipe(ImDrawList* dl, ImVec2 origin, ImVec2 size, float pixelScale) {
    if (!swipe_ || !s_) return;
    MapView& v = swipeView_;
    const SeriesLayer* L = nullptr;
    for (const SeriesLayer& c : layers_)
        if (c.session->info->id == v.cube) L = &c;
    if (!L) return; // closed: the bar picks another layer next frame
    const Session& S = *L->session;
    const CubeInfo& li = *S.info;
    const int T = li.T();
    const bool isActive = L == activeLayer();
    const int mode = v.ownMode ? v.mode : isActive ? mode_ : L->disp.mode;
    const int t = v.ownDate ? std::clamp(v.t, 0, T - 1) : isActive ? t_ : std::clamp(L->disp.t, 0, T - 1);

    // Detail tiles of the comparison, as in a map panel (the layer's own pixels).
    int level = -1;
    if (detail_ && mode == ModeValue && !S.deferRandomReads()) {
        const ViewRect r{(-offset_.x / scale_ - L->ax) / L->bx, (-offset_.y / scale_ - L->ay) / L->by,
                         ((size.x - offset_.x) / scale_ - L->ax) / L->bx, ((size.y - offset_.y) / scale_ - L->ay) / L->by,
                         scale_ * L->bx * pixelScale};
        level = S.tiles->update(t, r, -1);
    }
    if (level != v.detailLevel) {
        v.detailLevel = level;
        v.dirty = true;
    }
    if (size.x != v.size.x || size.y != v.size.y || pixelScale != v.pixelScale) v.dirty = true;
    if (v.dirty || viewsStale_) renderSwipe(size, pixelScale); // viewsStale_: the main map was redrawn

    // The comparison right of the divider: same view, so the same texture coordinates.
    const float x = origin.x + size.x * swipeX_;
    const bool bottomUp = Gpu::mapBottomUp();
    dl->AddImage(ImTextureRef((ImTextureID)gpu_.mapTexture(kSwipeSlot)), ImVec2(x, origin.y), origin + size,
                 ImVec2(swipeX_, bottomUp ? 1.f : 0.f), ImVec2(1, bottomUp ? 0.f : 1.f));
    dl->AddLine(ImVec2(x, origin.y), ImVec2(x, origin.y + size.y), IM_COL32(0, 0, 0, 200), 3.0f);
    dl->AddLine(ImVec2(x, origin.y), ImVec2(x, origin.y + size.y), IM_COL32(255, 255, 255, 230), 1.0f);
    const ImVec2 c(x, origin.y + size.y * 0.5f);
    dl->AddCircleFilled(c, 12.0f, IM_COL32(0, 0, 0, 180));
    dl->AddCircle(c, 12.0f, IM_COL32(255, 255, 255, 230), 0, 1.5f);
    dl->AddTriangleFilled(c + ImVec2(-3, -5), c + ImVec2(-3, 5), c + ImVec2(-8, 0), IM_COL32(255, 255, 255, 230));
    dl->AddTriangleFilled(c + ImVec2(3, -5), c + ImVec2(8, 0), c + ImVec2(3, 5), IM_COL32(255, 255, 255, 230));

    // What the right side shows, at the top right (below the overview progress bar).
    char title[200];
    if (mode == ModeRGB)
        std::snprintf(title, sizeof(title), "%s  |  RGB", L->name.c_str());
    else if (mode == ModeValue || mode == ModeAnomaly)
        std::snprintf(title, sizeof(title), "%s  |  %s  |  %s", L->name.c_str(), li.layers[t].label.c_str(), modeName(mode));
    else
        std::snprintf(title, sizeof(title), "%s  |  %s", L->name.c_str(), modeName(mode));
    const ImVec2 ts = ImGui::CalcTextSize(title);
    const ImVec2 tp = origin + ImVec2(std::max(x - origin.x + 10, size.x - ts.x - 10), s_->overview.complete() ? 8.f : 34.f);
    dl->AddText(tp + ImVec2(1, 1), IM_COL32(0, 0, 0, 200), title);
    dl->AddText(tp, IM_COL32(255, 255, 255, 255), title);
}

// ---------------------------------------------------------------------------
// View menu and keys (swipe and transect)
// ---------------------------------------------------------------------------

void App::uiCompareMenu() {
    ImGui::Separator();
    if (ImGui::MenuItem("Swipe", "S", swipe_, s_ != nullptr)) toggleSwipe();
    if (ImGui::MenuItem("Draw transect", "T or Ctrl+drag", transectMode_, s_ != nullptr)) transectMode_ = !transectMode_;
    if (ImGui::MenuItem("Clear transect", nullptr, false, tr_.on)) clearTransect();
    ImGui::Separator();
}

// Keys alone, without modifiers (Ctrl+T is a new map panel).
void App::compareShortcuts() {
    if (ImGui::IsKeyChordPressed(ImGuiKey_S)) toggleSwipe();
    if (ImGui::IsKeyChordPressed(ImGuiKey_T)) transectMode_ = !transectMode_;
    if (ImGui::IsKeyPressed(ImGuiKey_Escape, false)) {
        transectMode_ = false;
        if (transectPanel_ >= 0) transectCancel_ = true;
    }
}

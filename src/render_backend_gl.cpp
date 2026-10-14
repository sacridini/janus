// OpenGL 3.3 core backend of render_backend.hpp.
#include "render_backend.hpp"

#include "gl.hpp"

#include <imgui.h>
#include <imgui_impl_glfw.h>
#include <imgui_impl_opengl3.h>

#include <algorithm>
#include <vector>

namespace render {

const char* name() { return "OpenGL"; }

GLFWwindow* createWindow(int w, int h, const char* title, bool visible, std::string& error) {
    glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
    glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 3);
    glfwWindowHint(GLFW_OPENGL_PROFILE, GLFW_OPENGL_CORE_PROFILE);
#ifdef __APPLE__
    glfwWindowHint(GLFW_OPENGL_FORWARD_COMPAT, GLFW_TRUE); // required for a core profile on macOS
#endif
    glfwWindowHint(GLFW_MAXIMIZED, GLFW_TRUE);
    // An alpha channel the system composites (premultiplied): translucent panels
    // (Settings) show the desktop. Opaque as long as everything is drawn opaque;
    // the hint stays for the detached panels' windows, created later.
    glfwWindowHint(GLFW_TRANSPARENT_FRAMEBUFFER, GLFW_TRUE);
    if (!visible) glfwWindowHint(GLFW_VISIBLE, GLFW_FALSE); // never shown, never takes focus
    GLFWwindow* window = glfwCreateWindow(w, h, title, nullptr, nullptr);
    if (!window) {
        error = "could not create an OpenGL 3.3 window";
        return nullptr;
    }
    glfwMakeContextCurrent(window);
    glfwSwapInterval(1);
    if (!gladLoadGL(glfwGetProcAddress)) {
        error = "could not load OpenGL";
        glfwDestroyWindow(window);
        return nullptr;
    }
    return window;
}

namespace {
void (*g_renderWindow)(ImGuiViewport*, void*) = nullptr;

// A detached panel's window: cleared transparent (ImGui's backend clears it to
// opaque black), so translucent panels show the desktop there as in the main
// window; its panels' backgrounds cover it when they are opaque.
void renderWindow(ImGuiViewport* viewport, void* arg) {
    glDisable(GL_SCISSOR_TEST);
    glClearColor(0, 0, 0, 0);
    glClear(GL_COLOR_BUFFER_BIT);
    viewport->Flags |= ImGuiViewportFlags_NoRendererClear;
    g_renderWindow(viewport, arg);
}
} // namespace

bool initImGui(GLFWwindow* window) {
    if (!ImGui_ImplGlfw_InitForOpenGL(window, true) || !ImGui_ImplOpenGL3_Init("#version 330")) return false;
    ImGuiPlatformIO& pio = ImGui::GetPlatformIO();
    if (pio.Renderer_RenderWindow) {
        g_renderWindow = pio.Renderer_RenderWindow;
        pio.Renderer_RenderWindow = renderWindow;
    }
    return true;
}

void newFrame() {
    ImGui_ImplOpenGL3_NewFrame();
    ImGui_ImplGlfw_NewFrame();
}

void present(GLFWwindow* window, const float clear[4], const Capture& capture) {
    int fbw, fbh;
    glfwGetFramebufferSize(window, &fbw, &fbh);
    glViewport(0, 0, fbw, fbh);
    glClearColor(clear[0], clear[1], clear[2], clear[3]);
    glClear(GL_COLOR_BUFFER_BIT);
    ImGui_ImplOpenGL3_RenderDrawData(ImGui::GetDrawData());
    if (capture && fbw > 0 && fbh > 0) {
        std::vector<unsigned char> px(size_t(fbw) * fbh * 4), row(size_t(fbw) * 4);
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        glReadBuffer(GL_BACK);
        glReadPixels(0, 0, fbw, fbh, GL_RGBA, GL_UNSIGNED_BYTE, px.data());
        for (int y = 0; y < fbh / 2; ++y) { // OpenGL's first row is the bottom one
            unsigned char* a = &px[size_t(y) * fbw * 4];
            unsigned char* b = &px[size_t(fbh - 1 - y) * fbw * 4];
            std::copy(a, a + row.size(), row.data());
            std::copy(b, b + row.size(), a);
            std::copy(row.begin(), row.end(), b);
        }
        capture(px.data(), fbw, fbh);
    }
    // Panels detached to other windows/monitors (they share this GL context's
    // textures, so the map framebuffer can be shown there too).
    if (ImGui::GetIO().ConfigFlags & ImGuiConfigFlags_ViewportsEnable) {
        GLFWwindow* current = glfwGetCurrentContext();
        ImGui::UpdatePlatformWindows();
        ImGui::RenderPlatformWindowsDefault();
        glfwMakeContextCurrent(current);
    }
    glfwSwapBuffers(window);
}

void shutdownImGui() {
    ImGui_ImplOpenGL3_Shutdown();
    ImGui_ImplGlfw_Shutdown();
}

} // namespace render

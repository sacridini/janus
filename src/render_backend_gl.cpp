// OpenGL 3.3 core backend of render_backend.hpp.
#include "render_backend.hpp"

#include "gl.hpp"

#include <imgui.h>
#include <imgui_impl_glfw.h>
#include <imgui_impl_opengl3.h>

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

bool initImGui(GLFWwindow* window) {
    return ImGui_ImplGlfw_InitForOpenGL(window, true) && ImGui_ImplOpenGL3_Init("#version 330");
}

void newFrame() {
    ImGui_ImplOpenGL3_NewFrame();
    ImGui_ImplGlfw_NewFrame();
}

void present(GLFWwindow* window, const float clear[4]) {
    int fbw, fbh;
    glfwGetFramebufferSize(window, &fbw, &fbh);
    glViewport(0, 0, fbw, fbh);
    glClearColor(clear[0], clear[1], clear[2], clear[3]);
    glClear(GL_COLOR_BUFFER_BIT);
    ImGui_ImplOpenGL3_RenderDrawData(ImGui::GetDrawData());
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

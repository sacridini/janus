#pragma once

#include <string>

struct GLFWwindow;

// Window, graphics context and the Dear ImGui renderer backend: OpenGL 3.3
// (render_backend_gl.cpp) or Metal (render_backend_metal.mm), the same choice
// as gpu.hpp. main() owns the loop; this is everything API-specific around it.
namespace render {

const char* name(); // "OpenGL" or "Metal"

// After glfwInit: window hints of the API, the window and its context/layer.
GLFWwindow* createWindow(int w, int h, const char* title, bool visible, std::string& error);
// After ImGui::CreateContext: platform (GLFW) and renderer backends.
bool initImGui(GLFWwindow* window);
void newFrame();
// After ImGui::Render: draws the main window and the detached panels, presents.
void present(GLFWwindow* window, const float clear[4]);
void shutdownImGui();

} // namespace render

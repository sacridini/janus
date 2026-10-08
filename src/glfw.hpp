#pragma once

// GLFW without any graphics API header: windows, events and glfwPostEmptyEvent
// for renderer-neutral code (the OpenGL loader is gl.hpp, GL backend only).
#define GLFW_INCLUDE_NONE
#include <GLFW/glfw3.h>

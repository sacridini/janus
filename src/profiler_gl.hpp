#pragma once

// Tracy GPU zones for the OpenGL renderer (see profiler.hpp): timer queries on
// the main window's context, the one the map and the statistics are drawn in.
//
//   PROFILE_GPU_CONTEXT()        once, with the context current
//   PROFILE_GPU_ZONE("name")     GPU time of the enclosing scope's commands
//   PROFILE_GPU_COLLECT()        each frame, after the swap (that context current)

#include "gl.hpp"
#include "profiler.hpp"

#ifdef TRACY_ENABLE
#include <tracy/TracyOpenGL.hpp>
#define PROFILE_GPU_CONTEXT() TracyGpuContext
#define PROFILE_GPU_ZONE(name) TracyGpuZone(name)
#define PROFILE_GPU_COLLECT() TracyGpuCollect
#else
#define PROFILE_GPU_CONTEXT()
#define PROFILE_GPU_ZONE(name)
#define PROFILE_GPU_COLLECT()
#endif

#pragma once

// Tracy profiler zones (https://github.com/wolfpld/tracy). Built in with
// -DJANUS_TRACY=ON, which defines TRACY_ENABLE; otherwise every macro compiles
// to nothing. GPU zones (OpenGL) are in profiler_gl.hpp.
//
//   PROFILE_ZONE()           the enclosing scope, named after the function
//   PROFILE_ZONE_N("name")   the enclosing scope, with a name
//   PROFILE_TEXT(s, n)       text shown on the current zone (n bytes)
//   PROFILE_VALUE(v)         a number shown on the current zone
//   PROFILE_FRAME()          end of a UI frame
//   PROFILE_PLOT("name", v)  a value plotted over time
//   PROFILE_THREAD("name")   the calling thread's name in the timeline
//   PROFILE_MESSAGE(s)       a message in the timeline (s: a string that outlives the program)
//   PROFILE_SPAN_BEGIN(s)    a span over several frames (s: the same pointer to END)
//   PROFILE_SPAN_END(s)

#ifdef TRACY_ENABLE
#include <tracy/Tracy.hpp>
#define PROFILE_ZONE() ZoneScoped
#define PROFILE_ZONE_N(name) ZoneScopedN(name)
#define PROFILE_TEXT(s, n) ZoneText(s, n)
#define PROFILE_VALUE(v) ZoneValue(v)
#define PROFILE_FRAME() FrameMark
#define PROFILE_PLOT(name, v) TracyPlot(name, v)
#define PROFILE_THREAD(name) tracy::SetThreadName(name)
#define PROFILE_MESSAGE(s) TracyMessageL(s)
#define PROFILE_SPAN_BEGIN(s) FrameMarkStart(s)
#define PROFILE_SPAN_END(s) FrameMarkEnd(s)
#else
#define PROFILE_ZONE()
#define PROFILE_ZONE_N(name)
#define PROFILE_TEXT(s, n)
#define PROFILE_VALUE(v)
#define PROFILE_FRAME()
#define PROFILE_PLOT(name, v)
#define PROFILE_THREAD(name)
#define PROFILE_MESSAGE(s)
#define PROFILE_SPAN_BEGIN(s)
#define PROFILE_SPAN_END(s)
#endif

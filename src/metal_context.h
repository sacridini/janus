#pragma once

// Metal device and command queue shared by gpu_metal.mm (map, statistics,
// textures) and render_backend_metal.mm (ImGui, present). One queue: the map
// drawn in a frame is finished before the ImGui pass that shows it.
#import <Metal/Metal.h>

id<MTLDevice> tsvMetalDevice();
id<MTLCommandQueue> tsvMetalQueue();

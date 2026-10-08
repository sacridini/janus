// Metal backend of render_backend.hpp (macOS): a GLFW window without a GL
// context, drawn through a CAMetalLayer at the backing (Retina) resolution.
#include "render_backend.hpp"

#define GLFW_INCLUDE_NONE
#define GLFW_EXPOSE_NATIVE_COCOA
#include <GLFW/glfw3.h>
#include <GLFW/glfw3native.h>

#import <QuartzCore/QuartzCore.h>

#include <imgui.h>
#include <imgui_impl_glfw.h>
#include <imgui_impl_metal.h>

#include "metal_context.h"

namespace {

CAMetalLayer* g_layer = nil;
MTLRenderPassDescriptor* g_pass = nil;
// ImGui_ImplMetal_NewFrame takes the pixel format from the pass descriptor's
// texture; the drawable is only acquired at present (less latency, and none
// at all while the window is hidden or occluded), so a 1x1 stand-in is used.
id<MTLTexture> g_formatTexture = nil;
bool g_presented = false;

} // namespace

namespace render {

const char* name() { return "Metal"; }

GLFWwindow* createWindow(int w, int h, const char* title, bool visible, std::string& error) {
    glfwWindowHint(GLFW_CLIENT_API, GLFW_NO_API);
    glfwWindowHint(GLFW_MAXIMIZED, GLFW_TRUE);
    if (!visible) glfwWindowHint(GLFW_VISIBLE, GLFW_FALSE); // never shown, never takes focus
    id<MTLDevice> dev = janusMetalDevice();
    if (!dev) {
        error = "no Metal device";
        return nullptr;
    }
    GLFWwindow* window = glfwCreateWindow(w, h, title, nullptr, nullptr);
    if (!window) {
        error = "could not create the window";
        return nullptr;
    }
    NSWindow* nswin = glfwGetCocoaWindow(window);
    g_layer = [CAMetalLayer layer];
    g_layer.device = dev;
    g_layer.pixelFormat = MTLPixelFormatBGRA8Unorm;
    g_layer.framebufferOnly = YES;
    g_layer.displaySyncEnabled = YES; // vsync
    g_layer.contentsScale = nswin.backingScaleFactor;
    nswin.contentView.layer = g_layer;
    nswin.contentView.wantsLayer = YES;

    MTLTextureDescriptor* td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                  width:1
                                                                                 height:1
                                                                              mipmapped:NO];
    td.usage = MTLTextureUsageRenderTarget;
    td.storageMode = MTLStorageModePrivate;
    g_formatTexture = [dev newTextureWithDescriptor:td];
    g_pass = [MTLRenderPassDescriptor new];
    g_pass.colorAttachments[0].loadAction = MTLLoadActionClear;
    g_pass.colorAttachments[0].storeAction = MTLStoreActionStore;
    (void)janusMetalQueue(); // created now, not in the first frame
    return window;
}

bool initImGui(GLFWwindow* window) {
    // "Other": detached panels get GLFW windows without a GL context; the
    // Metal backend gives each one its own layer.
    return ImGui_ImplGlfw_InitForOther(window, true) && ImGui_ImplMetal_Init(janusMetalDevice());
}

void newFrame() {
    g_pass.colorAttachments[0].texture = g_formatTexture;
    ImGui_ImplMetal_NewFrame(g_pass);
    ImGui_ImplGlfw_NewFrame();
}

void present(GLFWwindow* window, const float clear[4]) {
    @autoreleasepool {
        NSWindow* nswin = glfwGetCocoaWindow(window);
        // Hidden (--selftest-ui) or fully covered: nextDrawable would block (up to
        // 1 s). A new window may not report itself visible yet: always draw it once.
        const bool visible =
            nswin.visible && (!g_presented || (nswin.occlusionState & NSWindowOcclusionStateVisible));
        if (visible) {
            int fbw, fbh;
            glfwGetFramebufferSize(window, &fbw, &fbh);
            const CGSize size = CGSizeMake(fbw, fbh);
            if (!CGSizeEqualToSize(g_layer.drawableSize, size)) g_layer.drawableSize = size;
            if (g_layer.contentsScale != nswin.backingScaleFactor) g_layer.contentsScale = nswin.backingScaleFactor;
            id<CAMetalDrawable> drawable = fbw > 0 && fbh > 0 ? [g_layer nextDrawable] : nil;
            if (drawable) {
                g_pass.colorAttachments[0].texture = drawable.texture;
                g_pass.colorAttachments[0].clearColor = MTLClearColorMake(clear[0], clear[1], clear[2], clear[3]);
                id<MTLCommandBuffer> cb = [janusMetalQueue() commandBuffer];
                id<MTLRenderCommandEncoder> enc = [cb renderCommandEncoderWithDescriptor:g_pass];
                ImGui_ImplMetal_RenderDrawData(ImGui::GetDrawData(), cb, enc);
                [enc endEncoding];
                [cb presentDrawable:drawable];
                [cb commit];
                g_presented = true;
            }
        }
        g_pass.colorAttachments[0].texture = nil; // do not keep the drawable alive
        // Panels detached to other windows/monitors (each with its own layer).
        if (ImGui::GetIO().ConfigFlags & ImGuiConfigFlags_ViewportsEnable) {
            ImGui::UpdatePlatformWindows();
            // Their command queues are not ordered with ours: the map drawn this
            // frame must be finished before one of them shows it.
            if (ImGui::GetPlatformIO().Viewports.Size > 1) {
                id<MTLCommandBuffer> fence = [janusMetalQueue() commandBuffer];
                [fence commit];
                [fence waitUntilCompleted];
            }
            ImGui::RenderPlatformWindowsDefault();
        }
    }
}

void shutdownImGui() {
    ImGui_ImplMetal_Shutdown();
    ImGui_ImplGlfw_Shutdown();
}

} // namespace render

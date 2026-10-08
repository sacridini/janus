cmake_minimum_required(VERSION 3.21)
# macOS 15: the oldest the Zeit runtime's wheels support (rasterio: macosx_15_0).
if(CMAKE_HOST_SYSTEM_NAME STREQUAL "Darwin" AND NOT DEFINED CMAKE_OSX_DEPLOYMENT_TARGET)
  set(CMAKE_OSX_DEPLOYMENT_TARGET "15.0" CACHE STRING "Minimum macOS version")
endif()
project(janus VERSION 0.22.0 DESCRIPTION "Raster time series viewer" LANGUAGES C CXX)
if(APPLE)
  enable_language(OBJC OBJCXX) # src/platform_mac.mm (OBJC too: GLFW's .m files stay Objective-C)
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Renderer: Metal on macOS (OpenGL is deprecated there), OpenGL 3.3 elsewhere.
# -DJANUS_RENDERER=GL also builds the OpenGL path on macOS.
if(APPLE)
  set(JANUS_RENDERER METAL CACHE STRING "Renderer: METAL or GL")
else()
  set(JANUS_RENDERER GL CACHE STRING "Renderer: GL")
endif()
set_property(CACHE JANUS_RENDERER PROPERTY STRINGS METAL GL)
if(JANUS_RENDERER STREQUAL "METAL" AND NOT APPLE)
  message(FATAL_ERROR "JANUS_RENDERER=METAL needs macOS")
endif()
message(STATUS "Renderer: ${JANUS_RENDERER}")

include(FetchContent)

# --- GLFW (window + input; also ships the glad OpenGL loader in deps/) -------
set(GLFW_BUILD_DOCS     OFF CACHE BOOL "" FORCE)
set(GLFW_BUILD_TESTS    OFF CACHE BOOL "" FORCE)
set(GLFW_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(GLFW_INSTALL        OFF CACHE BOOL "" FORCE)
FetchContent_Declare(glfw
  GIT_REPOSITORY https://github.com/glfw/glfw.git
  GIT_TAG        3.4
  GIT_SHALLOW    TRUE)

# --- Dear ImGui (docking branch) + ImPlot ------------------------------------
FetchContent_Declare(imgui
  GIT_REPOSITORY https://github.com/ocornut/imgui.git
  GIT_TAG        v1.92.9b-docking
  GIT_SHALLOW    TRUE)
FetchContent_Declare(implot
  GIT_REPOSITORY https://github.com/epezent/implot.git
  GIT_TAG        v1.0
  GIT_SHALLOW    TRUE)

# --- JSON (bridge protocol with Zeit) -----------------------------------------
set(JSON_BuildTests OFF CACHE INTERNAL "")
FetchContent_Declare(nlohmann_json
  GIT_REPOSITORY https://github.com/nlohmann/json.git
  GIT_TAG        v3.11.3
  GIT_SHALLOW    TRUE)

FetchContent_MakeAvailable(glfw imgui implot nlohmann_json)

find_package(GDAL CONFIG REQUIRED)

add_library(imgui STATIC
  ${imgui_SOURCE_DIR}/imgui.cpp
  ${imgui_SOURCE_DIR}/imgui_draw.cpp
  ${imgui_SOURCE_DIR}/imgui_tables.cpp
  ${imgui_SOURCE_DIR}/imgui_widgets.cpp
  ${imgui_SOURCE_DIR}/backends/imgui_impl_glfw.cpp
  ${implot_SOURCE_DIR}/implot.cpp
  ${implot_SOURCE_DIR}/implot_items.cpp)
target_include_directories(imgui PUBLIC ${imgui_SOURCE_DIR} ${imgui_SOURCE_DIR}/backends ${implot_SOURCE_DIR})
target_link_libraries(imgui PUBLIC glfw)
if(JANUS_RENDERER STREQUAL "METAL")
  target_sources(imgui PRIVATE ${imgui_SOURCE_DIR}/backends/imgui_impl_metal.mm)
  set_source_files_properties(${imgui_SOURCE_DIR}/backends/imgui_impl_metal.mm PROPERTIES COMPILE_OPTIONS -fobjc-arc)
  target_link_libraries(imgui PUBLIC "-framework Metal" "-framework QuartzCore")
else()
  find_package(OpenGL REQUIRED)
  target_sources(imgui PRIVATE ${imgui_SOURCE_DIR}/backends/imgui_impl_opengl3.cpp)
  target_link_libraries(imgui PUBLIC OpenGL::GL)
endif()

# --- App ---------------------------------------------------------------------
set(JANUS_SOURCES
  src/main.cpp
  src/app.cpp
  src/app_basemap.cpp
  src/app_classes.cpp
  src/app_export.cpp
  src/app_fullres.cpp
  src/app_layers.cpp
  src/app_selftest.cpp
  src/app_swipe.cpp
  src/app_transect.cpp
  src/app_zeit.cpp
  src/file_browser.cpp
  src/results.cpp
  src/selftest.cpp
  src/zeit_client.cpp
  src/basemap.cpp
  src/cube.cpp
  src/fullres_cache.cpp
  src/overview.cpp
  src/platform.cpp
  src/reproject.cpp
  src/session.cpp
  src/stats.cpp
  src/tiles.cpp)

if(APPLE)
  list(APPEND JANUS_SOURCES src/platform_mac.mm) # trackpad gestures, Finder documents (Cocoa)
endif()

if(JANUS_RENDERER STREQUAL "METAL")
  set(JANUS_METAL_SOURCES src/gpu_metal.mm src/render_backend_metal.mm)
  set_source_files_properties(${JANUS_METAL_SOURCES} PROPERTIES COMPILE_OPTIONS -fobjc-arc)
  list(APPEND JANUS_SOURCES ${JANUS_METAL_SOURCES})
else()
  list(APPEND JANUS_SOURCES src/gl_impl.cpp src/gpu_gl.cpp src/render_backend_gl.cpp)
endif()

if(WIN32)
  configure_file(resources/janus.rc.in ${CMAKE_CURRENT_BINARY_DIR}/janus.rc @ONLY)
  list(APPEND JANUS_SOURCES ${CMAKE_CURRENT_BINARY_DIR}/janus.rc)
endif()

# WIN32 = GUI subsystem: no console window on double click. --help attaches to
# the caller's console (platform::attachParentConsole).
add_executable(janus WIN32 ${JANUS_SOURCES})
target_include_directories(janus PRIVATE ${glfw_SOURCE_DIR}/deps)
target_compile_definitions(janus PRIVATE JANUS_VERSION="${PROJECT_VERSION}")
target_link_libraries(janus PRIVATE imgui GDAL::GDAL nlohmann_json::nlohmann_json)
if(APPLE)
  target_link_libraries(janus PRIVATE "-framework AppKit")
endif()

if(UNIX AND NOT APPLE)
  # Libraries bundled next to the program (see cmake/package_linux.cmake) are
  # found first; the build tree keeps working with the original ones.
  set_target_properties(janus PROPERTIES BUILD_RPATH "\$ORIGIN/lib")
endif()

if(MSVC)
  target_compile_options(janus PRIVATE /utf-8 /W3)
  target_link_options(janus PRIVATE /ENTRY:mainCRTStartup)
endif()

if(WIN32)
  # jn.com: the command line (console launcher). In a terminal, "jn" finds
  # jn.com (PATHEXT); it prints --help/--version or opens the GUI (janus.exe).
  add_executable(jn_cli src/launcher.cpp)
  set_target_properties(jn_cli PROPERTIES OUTPUT_NAME jn SUFFIX ".com" PDB_NAME jn_cli)
  target_compile_definitions(jn_cli PRIVATE JANUS_VERSION="${PROJECT_VERSION}" UNICODE _UNICODE)
  target_compile_options(jn_cli PRIVATE /utf-8 /W3)
  add_dependencies(janus jn_cli)
endif()

if(WIN32)
  # Post-build: copy the GDAL DLLs (+ dependencies) and proj.db next to the .exe.
  get_target_property(GDAL_DLL GDAL::GDAL IMPORTED_LOCATION_RELEASE)
  get_filename_component(GDAL_BIN_DIR "${GDAL_DLL}" DIRECTORY)
  get_filename_component(GDAL_SHARE_DIR "${GDAL_BIN_DIR}/../share" ABSOLUTE)
  get_filename_component(MSVC_BIN_DIR "${CMAKE_CXX_COMPILER}" DIRECTORY)
  add_custom_command(TARGET janus POST_BUILD
    COMMAND ${CMAKE_COMMAND}
      -DEXE=$<TARGET_FILE:janus>
      -DDLL_DIR=${GDAL_BIN_DIR}
      -DDATA_DIR=${GDAL_SHARE_DIR}
      -DDUMPBIN=${MSVC_BIN_DIR}/dumpbin.exe
      -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/deploy_runtime.cmake
    VERBATIM)
endif()

# --- Zeit runtime -------------------------------------------------------------
# Private Python with Zeit, next to janus.exe in runtime/ (never on PATH). Built on
# demand, since it downloads Python and wheels:
#   cmake --build build --config Release --target zeit_runtime
# The bridge script is refreshed on every build, so editing it does not require
# rebuilding the runtime.
find_package(Python3 COMPONENTS Interpreter)
if(Python3_FOUND)
  add_custom_target(zeit_runtime
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/tools/build_zeit_runtime.py
            --out $<TARGET_FILE_DIR:janus>/runtime --cache ${CMAKE_BINARY_DIR}/_downloads
    COMMENT "Assembling the Zeit runtime (embeddable Python + Zeit)"
    VERBATIM)
endif()
# A target (not a POST_BUILD step) so it also runs when only the .py files changed.
file(GLOB ZEIT_BRIDGE_PY CONFIGURE_DEPENDS ${CMAKE_CURRENT_SOURCE_DIR}/zeit_bridge/*.py)
add_custom_target(zeit_bridge
  COMMAND ${CMAKE_COMMAND} -E make_directory $<TARGET_FILE_DIR:janus>/runtime
  COMMAND ${CMAKE_COMMAND} -E copy_if_different ${ZEIT_BRIDGE_PY} $<TARGET_FILE_DIR:janus>/runtime/
  COMMENT "Copying the Zeit bridge scripts"
  VERBATIM)
add_dependencies(janus zeit_bridge)

# --- macOS app -----------------------------------------------------------------
# cmake --build build --target package_macos  ->  dist/janus-<version>-macos-arm64.dmg (Janus.app)
if(APPLE AND Python3_FOUND)
  add_custom_target(package_macos
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/tools/build_zeit_runtime.py
            --out ${CMAKE_BINARY_DIR}/package/runtime --cache ${CMAKE_BINARY_DIR}/_downloads
    COMMAND ${CMAKE_COMMAND} -DEXE=$<TARGET_FILE:janus> -DVERSION=${PROJECT_VERSION} -DMIN_MACOS=${CMAKE_OSX_DEPLOYMENT_TARGET}
            -DGDAL_LIB=$<TARGET_FILE:GDAL::GDAL> -DRUNTIME=${CMAKE_BINARY_DIR}/package/runtime
            -DSRC=${CMAKE_CURRENT_SOURCE_DIR} -DOUT=${CMAKE_BINARY_DIR}/package
            -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/package_macos.cmake
    DEPENDS janus
    COMMENT "Building the macOS app (.dmg)"
    VERBATIM)
endif()

# --- Linux package -------------------------------------------------------------
# cmake --build build --target package_linux  ->  dist/janus-<version>-linux-x86_64.tar.xz
if(UNIX AND NOT APPLE AND Python3_FOUND)
  add_custom_target(package_linux
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/tools/build_zeit_runtime.py
            --out ${CMAKE_BINARY_DIR}/package/runtime --cache ${CMAKE_BINARY_DIR}/_downloads
    COMMAND ${CMAKE_COMMAND} -DEXE=$<TARGET_FILE:janus> -DVERSION=${PROJECT_VERSION} -DGDAL_LIB=$<TARGET_FILE:GDAL::GDAL>
            -DRUNTIME=${CMAKE_BINARY_DIR}/package/runtime -DSRC=${CMAKE_CURRENT_SOURCE_DIR}
            -DOUT=${CMAKE_BINARY_DIR}/package -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/package_linux.cmake
    DEPENDS janus
    COMMENT "Building the portable Linux package"
    VERBATIM)
endif()

# --- Installer (Inno Setup 6) ------------------------------------------------
# cmake --build build --config Release --target installer  ->  dist/janus-<version>-setup.exe
if(WIN32)
  find_program(ISCC_EXE ISCC
    PATHS "$ENV{LOCALAPPDATA}/Programs/Inno Setup 6" "C:/Program Files (x86)/Inno Setup 6" "C:/Program Files/Inno Setup 6")
  if(ISCC_EXE)
    # The installer gets its own, freshly assembled runtime (build/package), so
    # packaging never touches the dev runtime a running Janus may be using.
    add_custom_target(installer
      COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/tools/build_zeit_runtime.py
              --out ${CMAKE_BINARY_DIR}/package/runtime --cache ${CMAKE_BINARY_DIR}/_downloads
      COMMAND ${ISCC_EXE} /Q /DAppVersion=${PROJECT_VERSION} /DBuildDir=$<TARGET_FILE_DIR:janus>
              /DRuntimeDir=${CMAKE_BINARY_DIR}/package/runtime
              ${CMAKE_CURRENT_SOURCE_DIR}/installer/janus.iss
      DEPENDS janus jn_cli
      WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/installer
      COMMENT "Building the installer with Inno Setup"
      VERBATIM)
  else()
    message(STATUS "Inno Setup (ISCC) not found: 'installer' target unavailable")
  endif()
endif()

cmake_minimum_required(VERSION 3.21)
project(tsv VERSION 0.4.0 DESCRIPTION "Raster time series viewer" LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

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

find_package(OpenGL REQUIRED)
find_package(GDAL CONFIG REQUIRED)

add_library(imgui STATIC
  ${imgui_SOURCE_DIR}/imgui.cpp
  ${imgui_SOURCE_DIR}/imgui_draw.cpp
  ${imgui_SOURCE_DIR}/imgui_tables.cpp
  ${imgui_SOURCE_DIR}/imgui_widgets.cpp
  ${imgui_SOURCE_DIR}/backends/imgui_impl_glfw.cpp
  ${imgui_SOURCE_DIR}/backends/imgui_impl_opengl3.cpp
  ${implot_SOURCE_DIR}/implot.cpp
  ${implot_SOURCE_DIR}/implot_items.cpp)
target_include_directories(imgui PUBLIC ${imgui_SOURCE_DIR} ${imgui_SOURCE_DIR}/backends ${implot_SOURCE_DIR})
target_link_libraries(imgui PUBLIC glfw OpenGL::GL)

# --- App ---------------------------------------------------------------------
set(TSV_SOURCES
  src/main.cpp
  src/app.cpp
  src/app_zeit.cpp
  src/results.cpp
  src/selftest.cpp
  src/zeit_client.cpp
  src/cube.cpp
  src/gl_impl.cpp
  src/gpu.cpp
  src/overview.cpp
  src/platform.cpp
  src/session.cpp
  src/stats.cpp
  src/tiles.cpp)

if(WIN32)
  configure_file(resources/tsv.rc.in ${CMAKE_CURRENT_BINARY_DIR}/tsv.rc @ONLY)
  list(APPEND TSV_SOURCES ${CMAKE_CURRENT_BINARY_DIR}/tsv.rc)
endif()

# WIN32 = GUI subsystem: no console window on double click. --help attaches to
# the caller's console (platform::attachParentConsole).
add_executable(tsv WIN32 ${TSV_SOURCES})
target_include_directories(tsv PRIVATE ${glfw_SOURCE_DIR}/deps)
target_compile_definitions(tsv PRIVATE TSV_VERSION="${PROJECT_VERSION}")
target_link_libraries(tsv PRIVATE imgui GDAL::GDAL nlohmann_json::nlohmann_json)

if(MSVC)
  target_compile_options(tsv PRIVATE /utf-8 /W3)
  target_link_options(tsv PRIVATE /ENTRY:mainCRTStartup)
endif()

if(WIN32)
  # tsv.com: console launcher. In a terminal, "tsv" resolves to .com before
  # .exe; it prints --help/--version or opens the GUI.
  add_executable(tsv_cli src/launcher.cpp)
  set_target_properties(tsv_cli PROPERTIES OUTPUT_NAME tsv SUFFIX ".com" PDB_NAME tsv_cli)
  target_compile_definitions(tsv_cli PRIVATE TSV_VERSION="${PROJECT_VERSION}" UNICODE _UNICODE)
  target_compile_options(tsv_cli PRIVATE /utf-8 /W3)
  add_dependencies(tsv tsv_cli)
endif()

if(WIN32)
  # Post-build: copy the GDAL DLLs (+ dependencies) and proj.db next to the .exe.
  get_target_property(GDAL_DLL GDAL::GDAL IMPORTED_LOCATION_RELEASE)
  get_filename_component(GDAL_BIN_DIR "${GDAL_DLL}" DIRECTORY)
  get_filename_component(GDAL_SHARE_DIR "${GDAL_BIN_DIR}/../share" ABSOLUTE)
  get_filename_component(MSVC_BIN_DIR "${CMAKE_CXX_COMPILER}" DIRECTORY)
  add_custom_command(TARGET tsv POST_BUILD
    COMMAND ${CMAKE_COMMAND}
      -DEXE=$<TARGET_FILE:tsv>
      -DDLL_DIR=${GDAL_BIN_DIR}
      -DDATA_DIR=${GDAL_SHARE_DIR}
      -DDUMPBIN=${MSVC_BIN_DIR}/dumpbin.exe
      -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/deploy_runtime.cmake
    VERBATIM)
endif()

# --- Zeit runtime -------------------------------------------------------------
# Private Python with Zeit, next to tsv.exe in runtime/ (never on PATH). Built on
# demand, since it downloads Python and wheels:
#   cmake --build build --config Release --target zeit_runtime
# The bridge script is refreshed on every build, so editing it does not require
# rebuilding the runtime.
find_package(Python3 COMPONENTS Interpreter)
if(WIN32 AND Python3_FOUND)
  add_custom_target(zeit_runtime
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/tools/build_zeit_runtime.py
            --out $<TARGET_FILE_DIR:tsv>/runtime --cache ${CMAKE_BINARY_DIR}/_downloads
    COMMENT "Assembling the Zeit runtime (embeddable Python + Zeit)"
    VERBATIM)
endif()
add_custom_command(TARGET tsv POST_BUILD
  COMMAND ${CMAKE_COMMAND} -E make_directory $<TARGET_FILE_DIR:tsv>/runtime
  COMMAND ${CMAKE_COMMAND} -E copy_if_different ${CMAKE_CURRENT_SOURCE_DIR}/zeit_bridge/tsv_zeit_bridge.py
          $<TARGET_FILE_DIR:tsv>/runtime/tsv_zeit_bridge.py
  VERBATIM)

# --- Installer (Inno Setup 6) ------------------------------------------------
# cmake --build build --config Release --target installer  ->  dist/tsv-<version>-setup.exe
if(WIN32)
  find_program(ISCC_EXE ISCC
    PATHS "$ENV{LOCALAPPDATA}/Programs/Inno Setup 6" "C:/Program Files (x86)/Inno Setup 6" "C:/Program Files/Inno Setup 6")
  if(ISCC_EXE)
    # The installer gets its own, freshly assembled runtime (build/package), so
    # packaging never touches the dev runtime a running tsv may be using.
    add_custom_target(installer
      COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/tools/build_zeit_runtime.py
              --out ${CMAKE_BINARY_DIR}/package/runtime --cache ${CMAKE_BINARY_DIR}/_downloads
      COMMAND ${ISCC_EXE} /Q /DAppVersion=${PROJECT_VERSION} /DBuildDir=$<TARGET_FILE_DIR:tsv>
              /DRuntimeDir=${CMAKE_BINARY_DIR}/package/runtime
              ${CMAKE_CURRENT_SOURCE_DIR}/installer/tsv.iss
      DEPENDS tsv tsv_cli
      WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/installer
      COMMENT "Building the installer with Inno Setup"
      VERBATIM)
  else()
    message(STATUS "Inno Setup (ISCC) not found: 'installer' target unavailable")
  endif()
endif()

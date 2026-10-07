# Portable Linux package: dist/tsv-<version>-linux-x86_64.tar.xz
#
#   tsv-<version>/
#     tsv                 the program (RPATH $ORIGIN/lib)
#     lib/                GDAL and its whole chain (PROJ, GEOS, libtiff, curl...)
#                         plus the C++ runtime it was built with; system
#                         libraries (glibc, OpenGL drivers, X11) come from the OS
#     share/gdal, share/proj
#     runtime/            private Python + Zeit (tools/build_zeit_runtime.py)
#     tsv.desktop, tsv.png, README.md
#
# Usage (script mode): cmake -DEXE=... -DVERSION=... -DGDAL_LIB=<libgdal.so> -DRUNTIME=... -DSRC=... -DOUT=... -P package_linux.cmake

cmake_policy(VERSION 3.21...4.4)

# GDAL's installation prefix (e.g. the conda environment): its lib/ and share/.
get_filename_component(PREFIX "${GDAL_LIB}" DIRECTORY)
get_filename_component(PREFIX "${PREFIX}" DIRECTORY)

set(name "tsv-${VERSION}")
set(stage "${OUT}/${name}")
file(REMOVE_RECURSE "${stage}")
file(MAKE_DIRECTORY "${stage}/lib")

file(GET_RUNTIME_DEPENDENCIES
  EXECUTABLES "${EXE}"
  DIRECTORIES "${PREFIX}/lib"
  RESOLVED_DEPENDENCIES_VAR deps
  UNRESOLVED_DEPENDENCIES_VAR unresolved
  # Provided by every Linux system, and must match its kernel/drivers/display:
  # glibc, the OpenGL dispatch and drivers, X11/Wayland client libraries.
  PRE_EXCLUDE_REGEXES
    "^ld-linux" "^libc\\.so" "^libm\\.so" "^libdl\\.so" "^libpthread\\.so" "^librt\\.so" "^libutil\\.so"
    "^libresolv\\.so" "^libGL" "^libEGL" "^libOpenGL" "^libX" "^libxcb" "^libwayland" "^libxkbcommon"
  POST_EXCLUDE_REGEXES "^/lib" "^/usr/lib")

foreach(dep IN LISTS deps)
  # Copy the real file under the name the program asks for (no symlinks in the archive).
  get_filename_component(real "${dep}" REALPATH)
  get_filename_component(soname "${dep}" NAME)
  file(COPY_FILE "${real}" "${stage}/lib/${soname}")
endforeach()
list(LENGTH deps n)
message(STATUS "package_linux: ${n} libraries")
if(unresolved)
  message(WARNING "package_linux: libraries not found: ${unresolved}")
endif()

file(COPY "${EXE}" DESTINATION "${stage}")
# Only the bundled libraries: drop the build machine's library paths.
file(RPATH_SET FILE "${stage}/tsv" NEW_RPATH "$ORIGIN/lib")
file(COPY "${PREFIX}/share/proj/proj.db" DESTINATION "${stage}/share/proj")
file(COPY "${PREFIX}/share/gdal" DESTINATION "${stage}/share")
file(COPY "${RUNTIME}/" DESTINATION "${stage}/runtime")
file(COPY "${SRC}/resources/tsv.png" "${SRC}/README.md" DESTINATION "${stage}")
file(WRITE "${stage}/tsv.desktop"
  "[Desktop Entry]\nType=Application\nName=tsv\nComment=Raster time series viewer\n"
  "Exec=tsv %F\nIcon=tsv\nTerminal=false\nCategories=Science;Geography;Graphics;\n"
  "MimeType=image/tiff;\n")

file(MAKE_DIRECTORY "${SRC}/dist")
set(archive "${SRC}/dist/${name}-linux-x86_64.tar.xz")
execute_process(COMMAND ${CMAKE_COMMAND} -E tar cJf "${archive}" "${name}" WORKING_DIRECTORY "${OUT}" RESULT_VARIABLE rc)
if(NOT rc EQUAL 0)
  message(FATAL_ERROR "package_linux: tar failed (${rc})")
endif()
file(SIZE "${archive}" size)
math(EXPR mb "${size} / 1048576")
message(STATUS "package_linux: ${archive} (${mb} MB)")

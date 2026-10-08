# macOS app: dist/tsv-<version>-macos-arm64.dmg, holding tsv.app and a link to
# /Applications (drag to install).
#
#   tsv.app/Contents/
#     Info.plist          document types: "Open With tsv" for folders and rasters
#     MacOS/tsv           the program (RPATH @executable_path/../Frameworks)
#     Frameworks/         GDAL and its whole chain (PROJ, GEOS, libtiff, curl...);
#                         system libraries and frameworks come from macOS
#     Resources/          tsv.icns, share/gdal, share/proj, runtime/ (private
#                         Python + Zeit, tools/build_zeit_runtime.py)
#
# Signed ad hoc (runs on the machine that installs it; another Mac's Gatekeeper
# asks for confirmation the first time, see README).
#
# Usage (script mode): cmake -DEXE=... -DVERSION=... -DMIN_MACOS=... -DGDAL_LIB=<libgdal.dylib> -DRUNTIME=...
#                            -DSRC=... -DOUT=... -P package_macos.cmake

cmake_policy(VERSION 3.21...4.4)

# GDAL's installation prefix (e.g. the conda environment): its lib/ and share/.
get_filename_component(PREFIX "${GDAL_LIB}" DIRECTORY)
get_filename_component(PREFIX "${PREFIX}" DIRECTORY)

set(stage "${OUT}/dmg")
set(app "${stage}/tsv.app/Contents")
file(REMOVE_RECURSE "${stage}")
file(MAKE_DIRECTORY "${app}/MacOS" "${app}/Frameworks" "${app}/Resources/share/proj")

function(run)
  execute_process(COMMAND ${ARGN} RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE out)
  if(NOT rc EQUAL 0)
    message(FATAL_ERROR "package_macos: ${ARGN} failed (${rc}): ${out}")
  endif()
endfunction()

file(GET_RUNTIME_DEPENDENCIES
  EXECUTABLES "${EXE}"
  DIRECTORIES "${PREFIX}/lib"
  RESOLVED_DEPENDENCIES_VAR deps
  UNRESOLVED_DEPENDENCIES_VAR unresolved
  POST_EXCLUDE_REGEXES "^/usr/lib" "^/System")

foreach(dep IN LISTS deps)
  # The real file under the name the libraries ask for (@rpath/<name>).
  get_filename_component(real "${dep}" REALPATH)
  get_filename_component(name "${dep}" NAME)
  file(COPY_FILE "${real}" "${app}/Frameworks/${name}")
endforeach()
list(LENGTH deps n)
message(STATUS "package_macos: ${n} libraries")
if(unresolved)
  message(WARNING "package_macos: libraries not found: ${unresolved}")
endif()

file(COPY_FILE "${EXE}" "${app}/MacOS/tsv")
# Only the bundled libraries: the build machine's library path is replaced
# (conda's libraries find each other through @loader_path/).
run(install_name_tool -rpath "${PREFIX}/lib" "@executable_path/../Frameworks" "${app}/MacOS/tsv")

file(COPY "${PREFIX}/share/proj/proj.db" DESTINATION "${app}/Resources/share/proj")
file(COPY "${PREFIX}/share/gdal" DESTINATION "${app}/Resources/share")
file(COPY "${RUNTIME}/" DESTINATION "${app}/Resources/runtime")
file(COPY_FILE "${SRC}/resources/tsv.icns" "${app}/Resources/tsv.icns")
configure_file("${SRC}/resources/Info.plist.in" "${app}/Info.plist" @ONLY)

# install_name_tool invalidated the signatures (and on Apple Silicon unsigned
# code is killed on load): sign every library, then the bundle (seals Resources).
file(GLOB libs "${app}/Frameworks/*")
foreach(lib IN LISTS libs)
  run(codesign --force --sign - "${lib}")
endforeach()
run(codesign --force --sign - "${stage}/tsv.app")
run(codesign --verify --strict "${stage}/tsv.app")

file(CREATE_LINK /Applications "${stage}/Applications" SYMBOLIC)
file(MAKE_DIRECTORY "${SRC}/dist")
set(dmg "${SRC}/dist/tsv-${VERSION}-macos-arm64.dmg")
file(REMOVE "${dmg}")
# hdiutil fails now and then with "Resource busy" while the system still scans
# the new folder (seen on CI runners): try again a few times.
foreach(attempt RANGE 1 5)
  execute_process(COMMAND hdiutil create -volname "tsv ${VERSION}" -srcfolder "${stage}" -fs APFS -format ULMO -ov "${dmg}"
                  RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE out)
  if(rc EQUAL 0)
    break()
  endif()
  message(STATUS "package_macos: hdiutil failed (attempt ${attempt}): ${out}")
  execute_process(COMMAND sleep 10)
endforeach()
if(NOT rc EQUAL 0)
  message(FATAL_ERROR "package_macos: hdiutil create failed: ${out}")
endif()
file(SIZE "${dmg}" size)
math(EXPR mb "${size} / 1048576")
message(STATUS "package_macos: ${dmg} (${mb} MB)")

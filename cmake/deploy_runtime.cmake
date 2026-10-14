# Copies into the executable's folder every DLL it depends on (GDAL and its
# whole chain: PROJ, GEOS, libtiff, curl...) plus the PROJ/GDAL data. The .exe
# then runs on double click, without activating the conda environment and
# without picking up another GDAL installation that happens to be on PATH.
#
# Usage (script mode): cmake -DEXE=... -DDLL_DIR=... -DDATA_DIR=... -DDUMPBIN=... -P deploy_runtime.cmake

get_filename_component(OUT_DIR "${EXE}" DIRECTORY)

set(CMAKE_GET_RUNTIME_DEPENDENCIES_PLATFORM "windows+pe")
set(CMAKE_GET_RUNTIME_DEPENDENCIES_TOOL "dumpbin")
set(CMAKE_GET_RUNTIME_DEPENDENCIES_COMMAND "${DUMPBIN}")

# GDAL drivers built as plugins (conda-forge: libgdal-netcdf, libgdal-hdf5):
# NetCDF and HDF5 rasters. Copied to gdalplugins/ (main.cpp points GDAL there);
# their DLLs (netcdf, hdf5...) go next to the program with the rest.
set(plugin_dir "${DLL_DIR}/../lib/gdalplugins")
set(plugins "")
foreach(name gdal_netCDF gdal_HDF5)
  if(EXISTS "${plugin_dir}/${name}.dll")
    list(APPEND plugins "${plugin_dir}/${name}.dll")
  endif()
endforeach()
if(NOT plugins)
  message(WARNING "deploy_runtime: no GDAL netCDF/HDF5 plugin in ${plugin_dir} (conda install libgdal-netcdf): "
                  "NetCDF files will not open")
endif()

file(GET_RUNTIME_DEPENDENCIES
  EXECUTABLES "${EXE}"
  MODULES ${plugins}
  DIRECTORIES "${DLL_DIR}"
  RESOLVED_DEPENDENCIES_VAR deps
  UNRESOLVED_DEPENDENCIES_VAR unresolved
  CONFLICTING_DEPENDENCIES_PREFIX conflicts
  PRE_EXCLUDE_REGEXES "^api-ms-" "^ext-ms-"
  POST_EXCLUDE_REGEXES "[Ss]ystem32" "[Ss]ys[Ww][Oo][Ww]64")

# On a conflict (same DLL in more than one PATH entry), keep the one in DLL_DIR.
foreach(name IN LISTS conflicts_FILENAMES)
  foreach(candidate IN LISTS conflicts_${name})
    cmake_path(GET candidate PARENT_PATH parent)
    if(parent STREQUAL DLL_DIR)
      list(APPEND deps "${candidate}")
    endif()
  endforeach()
endforeach()

file(COPY ${deps} DESTINATION "${OUT_DIR}")
list(LENGTH deps n)
message(STATUS "deploy_runtime: copied ${n} DLLs to ${OUT_DIR}")
if(unresolved)
  message(WARNING "deploy_runtime: DLLs not found: ${unresolved}")
endif()

file(REMOVE_RECURSE "${OUT_DIR}/gdalplugins")
if(plugins)
  file(COPY ${plugins} DESTINATION "${OUT_DIR}/gdalplugins")
endif()

file(COPY "${DATA_DIR}/proj/proj.db" DESTINATION "${OUT_DIR}/share/proj")
file(COPY "${DATA_DIR}/gdal" DESTINATION "${OUT_DIR}/share")

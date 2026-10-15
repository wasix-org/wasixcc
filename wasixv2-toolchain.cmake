# CMake toolchain file for WASIX v2 (wasm32-wasixv2)
cmake_minimum_required(VERSION 3.5.0)

# WASIX v2 does not define __linux__ (yet), so don't present it as Linux.
set(CMAKE_SYSTEM_NAME WASI)
set(CMAKE_SYSTEM_VERSION 1)
set(CMAKE_SYSTEM_PROCESSOR wasm32)
set(CMAKE_C_COMPILER_ID Clang)
# CMake passes this as --target, which selects WASIX v2 in wasixcc.
set(triple wasm32-wasixv2)
set(CMAKE_C_COMPILER_TARGET ${triple})
set(CMAKE_CXX_COMPILER_TARGET ${triple})
set(CMAKE_ASM_COMPILER_TARGET ${triple})

# The sysroot is only used to find headers and libraries; wasixcc passes its
# own --sysroot to clang.
if(DEFINED ENV{WASIX_SYSROOT})
  set(CMAKE_SYSROOT "$ENV{WASIX_SYSROOT}")
else()
  execute_process(
    COMMAND wasixccenv -sTARGET=wasm32-wasixv2 print-sysroot
    OUTPUT_VARIABLE CMAKE_SYSROOT
    OUTPUT_STRIP_TRAILING_WHITESPACE
    RESULT_VARIABLE wasixv2_print_sysroot_result
  )
  if(NOT wasixv2_print_sysroot_result EQUAL 0)
    message(FATAL_ERROR "Could not locate the wasm32-wasixv2 sysroot; set WASIX_SYSROOT \
or run `wasixccenv -sTARGET=wasm32-wasixv2 download-sysroot`")
  endif()
endif()

set(CMAKE_C_COMPILER wasixcc)
set(CMAKE_CXX_COMPILER wasix++)
set(CMAKE_LINKER wasixld)
set(CMAKE_AR wasixar)
set(CMAKE_RANLIB wasixranlib)

# WASIX v2 has no dynamic linking yet.
set_property(GLOBAL PROPERTY TARGET_SUPPORTS_SHARED_LIBS FALSE)

# Don't look in the sysroot for executables to run during the build
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
# Only look in the sysroot (not in the host paths) for the rest
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

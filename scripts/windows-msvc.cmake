# Rust uses the release DLL CRT in both debug and release builds. Match it
# instead of CMake's default debug DLL CRT (MSVCRTD), which cannot be mixed in.
# https://cmake.org/cmake/help/latest/variable/CMAKE_MSVC_RUNTIME_LIBRARY.html
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL" CACHE STRING "MSVC runtime matching Rust" FORCE)

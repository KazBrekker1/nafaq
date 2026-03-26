# Wrapper: sets the correct ABI then delegates to the NDK toolchain.
# cmake-rs passes -DCMAKE_SYSTEM_NAME=Android but not ANDROID_ABI,
# so the NDK toolchain would default to armeabi-v7a (32-bit).
set(ANDROID_ABI "arm64-v8a")
set(ANDROID_PLATFORM "android-24")
include("$ENV{ANDROID_NDK_HOME}/build/cmake/android.toolchain.cmake")

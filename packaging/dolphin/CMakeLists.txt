cmake_minimum_required(VERSION 3.16)
project(syncfast-dolphin VERSION 1.0.0 LANGUAGES CXX)

set(KDE_INSTALL_USE_QT_SYS_PATHS ON CACHE BOOL "Install the plugins where Qt looks for them")

find_package(ECM 6.0 REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

include(KDEInstallDirs)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)
include(ECMAddTests)

find_package(Qt6 6.6 REQUIRED COMPONENTS Core DBus Widgets Test)
find_package(KF6 6.0 REQUIRED COMPONENTS CoreAddons KIO)

add_library(syncfast_dolphin STATIC src/status.cpp src/bus.cpp src/overlay.cpp)
set_target_properties(syncfast_dolphin PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_include_directories(syncfast_dolphin PUBLIC src)
target_link_libraries(syncfast_dolphin PUBLIC Qt6::Core Qt6::DBus KF6::KIOCore)

kcoreaddons_add_plugin(syncfastoverlay SOURCES src/overlayplugin.cpp INSTALL_NAMESPACE "kf6/overlayicon")
target_link_libraries(syncfastoverlay syncfast_dolphin KF6::CoreAddons KF6::KIOCore)

kcoreaddons_add_plugin(syncfastactions SOURCES src/actions.cpp INSTALL_NAMESPACE "kf6/kfileitemaction")
target_link_libraries(syncfastactions syncfast_dolphin KF6::CoreAddons KF6::KIOCore KF6::KIOWidgets Qt6::Widgets)

if(BUILD_TESTING)
    ecm_add_tests(tests/test_status.cpp tests/test_overlay.cpp LINK_LIBRARIES syncfast_dolphin Qt6::Test)
endif()

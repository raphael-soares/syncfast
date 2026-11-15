#include "overlay.h"

class SyncfastOverlayPlugin : public SyncfastOverlay
{
    Q_PLUGIN_METADATA(IID "io.github.raphael_soares.syncfast.overlayicon" FILE "syncfastoverlay.json")
    Q_OBJECT
};

#include "overlayplugin.moc"

#pragma once

#include "status.h"

#include <KOverlayIconPlugin>
#include <QSet>

class SyncfastOverlay : public KOverlayIconPlugin
{
    Q_OBJECT

public:
    explicit SyncfastOverlay(QObject *parent = nullptr);

    QStringList getOverlays(const QUrl &url) override;

private Q_SLOTS:
    void onAppeared();
    void onVanished();
    void onChanged();

private:
    QStringList overlaysFor(const QString &path);
    void refresh();
    void request(const QString &dir, bool again = false);
    void replace(const syncfast::StatusCache &cache);
    void announce(const QStringList &paths);

    syncfast::StatusCache m_cache;
    syncfast::DirectoryCache m_entries;
    QSet<QString> m_inflight;
    QSet<QString> m_stale;
    int m_generation = 0;
    int m_directoryGeneration = 0;
    bool m_connected = false;
};

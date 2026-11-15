#include "overlay.h"

#include "bus.h"

#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusServiceWatcher>
#include <QUrl>

using namespace syncfast;

namespace
{

QDBusMessage methodCall(const QString &method)
{
    return QDBusMessage::createMethodCall(BusName, ObjectPath, Interface, method);
}

}

SyncfastOverlay::SyncfastOverlay(QObject *parent)
    : KOverlayIconPlugin(parent)
{
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.isConnected()) {
        return;
    }
    auto *watcher = new QDBusServiceWatcher(BusName,
                                            bus,
                                            QDBusServiceWatcher::WatchForRegistration | QDBusServiceWatcher::WatchForUnregistration,
                                            this);
    connect(watcher, &QDBusServiceWatcher::serviceRegistered, this, &SyncfastOverlay::onAppeared);
    connect(watcher, &QDBusServiceWatcher::serviceUnregistered, this, &SyncfastOverlay::onVanished);
    if (bus.interface()->isServiceRegistered(BusName)) {
        onAppeared();
    }
}

QStringList SyncfastOverlay::getOverlays(const QUrl &url)
{
    if (!url.isLocalFile()) {
        return {};
    }
    const QString path = normalized(url.toLocalFile());
    if (rootIcon(m_cache.rootState(path)).isEmpty() && m_cache.rootOf(path)) {
        const QString dir = parentOf(path);
        if (!m_entries.known(dir)) {
            request(dir);
        }
    }
    return overlaysFor(path);
}

QStringList SyncfastOverlay::overlaysFor(const QString &path)
{
    const QString root = rootIcon(m_cache.rootState(path));
    if (!root.isEmpty()) {
        return {root};
    }
    if (!m_cache.rootOf(path)) {
        return {};
    }
    const std::optional<QString> state = m_entries.entry(path);
    if (!state) {
        return {};
    }
    const QString icon = entryIcon(*state);
    if (icon.isEmpty()) {
        return {};
    }
    return {icon};
}

void SyncfastOverlay::onAppeared()
{
    if (!m_connected) {
        QDBusConnection::sessionBus().connect(BusName, ObjectPath, Interface, QStringLiteral("Changed"), this, SLOT(onChanged()));
        m_connected = true;
    }
    onChanged();
}

void SyncfastOverlay::onVanished()
{
    if (m_connected) {
        QDBusConnection::sessionBus().disconnect(BusName, ObjectPath, Interface, QStringLiteral("Changed"), this, SLOT(onChanged()));
    }
    m_connected = false;
    ++m_generation;
    ++m_directoryGeneration;
    m_inflight.clear();
    m_stale.clear();
    replace(StatusCache());
    announce(m_entries.dropAll());
}

void SyncfastOverlay::onChanged()
{
    refresh();
    QSet<QString> dirs = m_inflight;
    for (const QString &dir : m_entries.directories()) {
        dirs.insert(dir);
    }
    for (const QString &dir : std::as_const(dirs)) {
        request(dir, true);
    }
}

void SyncfastOverlay::refresh()
{
    if (!m_connected) {
        return;
    }
    const int generation = ++m_generation;
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(methodCall(QStringLiteral("Roots"))), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, generation](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        if (generation != m_generation) {
            return;
        }
        const QDBusMessage reply = call->reply();
        if (reply.type() != QDBusMessage::ReplyMessage) {
            replace(StatusCache());
            return;
        }
        replace(StatusCache(readRoots(reply), true));
    });
}

void SyncfastOverlay::request(const QString &dir, bool again)
{
    if (!m_connected) {
        return;
    }
    if (m_inflight.contains(dir)) {
        if (again) {
            m_stale.insert(dir);
        }
        return;
    }
    m_inflight.insert(dir);
    const int generation = m_directoryGeneration;
    QDBusMessage message = methodCall(QStringLiteral("Directory"));
    message << dir;
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(message), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, generation, dir](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        if (generation != m_directoryGeneration) {
            return;
        }
        m_inflight.remove(dir);
        announce(m_entries.store(dir, readEntries(call->reply())));
        if (m_stale.remove(dir)) {
            request(dir);
        }
    });
}

void SyncfastOverlay::replace(const StatusCache &cache)
{
    const StatusCache old = std::exchange(m_cache, cache);
    announce(changedPaths(old, m_cache));
}

void SyncfastOverlay::announce(const QStringList &paths)
{
    for (const QString &path : paths) {
        Q_EMIT overlaysChanged(QUrl::fromLocalFile(path), overlaysFor(path));
    }
}

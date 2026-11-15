#include "status.h"

#include <QSet>

#include <algorithm>

namespace syncfast
{

namespace
{

QString join(const QString &dir, const QString &name)
{
    if (dir == QLatin1String("/")) {
        return QLatin1Char('/') + name;
    }
    return dir + QLatin1Char('/') + name;
}

std::pair<QString, QString> split(const QString &path)
{
    return {parentOf(path), path.mid(path.lastIndexOf(QLatin1Char('/')) + 1)};
}

}

QString parentOf(const QString &path)
{
    const qsizetype slash = path.lastIndexOf(QLatin1Char('/'));
    if (slash < 0) {
        return {};
    }
    return slash == 0 ? QStringLiteral("/") : path.left(slash);
}

bool inside(const QString &root, const QString &path)
{
    QString trimmed = root;
    while (trimmed.endsWith(QLatin1Char('/'))) {
        trimmed.chop(1);
    }
    return path == trimmed || path.startsWith(trimmed + QLatin1Char('/'));
}

QString normalized(const QString &path)
{
    QString result = path;
    while (result.size() > 1 && result.endsWith(QLatin1Char('/'))) {
        result.chop(1);
    }
    return result;
}

QString rootIcon(const QString &state)
{
    if (state == QLatin1String("synced")) {
        return QStringLiteral("vcs-normal");
    }
    if (state == QLatin1String("pending") || state == QLatin1String("syncing")) {
        return QStringLiteral("vcs-update-required");
    }
    if (state == QLatin1String("conflict") || state == QLatin1String("attention")) {
        return QStringLiteral("vcs-conflicting");
    }
    return {};
}

QString entryIcon(const QString &state)
{
    if (state == QLatin1String("synced")) {
        return QStringLiteral("vcs-normal");
    }
    if (state == QLatin1String("pending") || state == QLatin1String("syncing")) {
        return QStringLiteral("vcs-update-required");
    }
    if (state == QLatin1String("error") || state == QLatin1String("conflict")) {
        return QStringLiteral("vcs-conflicting");
    }
    return {};
}

StatusCache::StatusCache(const Pairs &roots, bool available)
    : m_available(available)
{
    for (const auto &[path, state] : roots) {
        m_roots.insert(normalized(path), state);
    }
}

std::optional<QString> StatusCache::rootOf(const QString &path) const
{
    if (!m_available) {
        return std::nullopt;
    }
    std::optional<QString> found;
    for (auto it = m_roots.cbegin(); it != m_roots.cend(); ++it) {
        if (inside(it.key(), path) && (!found || it.key().size() > found->size())) {
            found = it.key();
        }
    }
    return found;
}

QString StatusCache::rootState(const QString &path) const
{
    return m_available ? m_roots.value(path) : QString();
}

std::optional<QString> StatusCache::syncTarget(const QStringList &paths) const
{
    for (const QString &path : paths) {
        if (rootOf(path)) {
            return path;
        }
    }
    return std::nullopt;
}

QStringList StatusCache::rootPaths() const
{
    return m_roots.keys();
}

QStringList changedPaths(const StatusCache &old, const StatusCache &now)
{
    const QStringList before = old.rootPaths();
    QSet<QString> candidates(before.cbegin(), before.cend());
    for (const QString &path : now.rootPaths()) {
        candidates.insert(path);
    }
    QStringList changed;
    for (const QString &path : std::as_const(candidates)) {
        if (old.rootState(path) != now.rootState(path)) {
            changed.append(path);
        }
    }
    changed.sort();
    return changed;
}

std::optional<QString> addSyncTarget(const StatusCache &cache, const QList<Selected> &items)
{
    if (items.size() != 1) {
        return std::nullopt;
    }
    const Selected &item = items.first();
    if (item.path.isEmpty() || !item.isFolder) {
        return std::nullopt;
    }
    const QStringList roots = cache.rootPaths();
    const bool overlaps = std::any_of(roots.cbegin(), roots.cend(), [&](const QString &root) {
        return inside(root, item.path) || inside(item.path, root);
    });
    if (overlaps) {
        return std::nullopt;
    }
    return item.path;
}

QStringList DirectoryCache::store(const QString &dir, const Pairs &entries)
{
    QHash<QString, QString> now;
    for (const auto &[name, state] : entries) {
        now.insert(name, state);
    }
    const QHash<QString, QString> old = m_folders.take(dir);
    m_folders.insert(dir, now);
    m_order.removeOne(dir);
    m_order.append(dir);
    while (m_order.size() > MaxDirectories) {
        m_folders.remove(m_order.takeFirst());
    }

    QSet<QString> names(old.keyBegin(), old.keyEnd());
    for (auto it = now.keyBegin(); it != now.keyEnd(); ++it) {
        names.insert(*it);
    }
    QStringList changed;
    for (const QString &name : std::as_const(names)) {
        if (old.contains(name) != now.contains(name) || old.value(name) != now.value(name)) {
            changed.append(join(dir, name));
        }
    }
    changed.sort();
    return changed;
}

std::optional<QString> DirectoryCache::entry(const QString &path)
{
    const auto [dir, name] = split(path);
    const auto folder = m_folders.constFind(dir);
    if (folder == m_folders.cend()) {
        return std::nullopt;
    }
    m_order.removeOne(dir);
    m_order.append(dir);
    const auto state = folder->constFind(name);
    if (state == folder->cend()) {
        return std::nullopt;
    }
    return *state;
}

bool DirectoryCache::known(const QString &dir) const
{
    return m_folders.contains(dir);
}

QStringList DirectoryCache::directories() const
{
    return m_order;
}

QStringList DirectoryCache::dropAll()
{
    QStringList paths;
    for (auto folder = m_folders.cbegin(); folder != m_folders.cend(); ++folder) {
        for (auto it = folder->keyBegin(); it != folder->keyEnd(); ++it) {
            paths.append(join(folder.key(), *it));
        }
    }
    m_folders.clear();
    m_order.clear();
    paths.sort();
    return paths;
}

}

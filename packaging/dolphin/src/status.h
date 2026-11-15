#pragma once

#include <QHash>
#include <QList>
#include <QString>
#include <QStringList>

#include <optional>
#include <utility>

namespace syncfast
{

inline const QString BusName = QStringLiteral("io.github.raphael_soares.syncfast.Status");
inline const QString ObjectPath = QStringLiteral("/io/github/raphael_soares/syncfast/Status");
inline const QString Interface = BusName;
inline const QString AppId = QStringLiteral("io.github.raphael_soares.syncfast");
constexpr int MaxDirectories = 64;

using Pairs = QList<std::pair<QString, QString>>;

bool inside(const QString &root, const QString &path);
QString normalized(const QString &path);
QString parentOf(const QString &path);
QString rootIcon(const QString &state);
QString entryIcon(const QString &state);

class StatusCache
{
public:
    StatusCache() = default;
    StatusCache(const Pairs &roots, bool available);

    std::optional<QString> rootOf(const QString &path) const;
    QString rootState(const QString &path) const;
    std::optional<QString> syncTarget(const QStringList &paths) const;
    QStringList rootPaths() const;

private:
    QHash<QString, QString> m_roots;
    bool m_available = false;
};

QStringList changedPaths(const StatusCache &old, const StatusCache &now);

struct Selected {
    QString path;
    bool isFolder;
};

std::optional<QString> addSyncTarget(const StatusCache &cache, const QList<Selected> &items);

class DirectoryCache
{
public:
    QStringList store(const QString &dir, const Pairs &entries);
    std::optional<QString> entry(const QString &path);
    bool known(const QString &dir) const;
    QStringList directories() const;
    QStringList dropAll();

private:
    QStringList m_order;
    QHash<QString, QHash<QString, QString>> m_folders;
};

}

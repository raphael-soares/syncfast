#include "bus.h"
#include "status.h"

#include <KAbstractFileItemActionPlugin>
#include <KFileItem>
#include <KFileItemListProperties>
#include <KPluginFactory>
#include <QAction>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCall>
#include <QProcess>
#include <QWidget>

using namespace syncfast;

class SyncfastActions : public KAbstractFileItemActionPlugin
{
    Q_OBJECT

public:
    SyncfastActions(QObject *parent, const QVariantList &)
        : KAbstractFileItemActionPlugin(parent)
    {
    }

    QList<QAction *> actions(const KFileItemListProperties &props, QWidget *parentWidget) override
    {
        if (!props.isLocal()) {
            return {};
        }
        QStringList paths;
        QList<Selected> items;
        const KFileItemList selection = props.items();
        for (const KFileItem &item : selection) {
            const QString path = normalized(item.url().toLocalFile());
            paths.append(path);
            items.append({path, item.isDir()});
        }

        const QDBusMessage reply = QDBusConnection::sessionBus().call(
            QDBusMessage::createMethodCall(BusName, ObjectPath, Interface, QStringLiteral("Roots")),
            QDBus::Block,
            300);
        const StatusCache cache = reply.type() == QDBusMessage::ReplyMessage ? StatusCache(readRoots(reply), true) : StatusCache();

        QList<QAction *> result;
        if (const std::optional<QString> target = cache.syncTarget(paths)) {
            auto *syncNow = new QAction(QStringLiteral("Sync Now"), parentWidget);
            connect(syncNow, &QAction::triggered, syncNow, [path = *target] {
                QDBusMessage message = QDBusMessage::createMethodCall(BusName, ObjectPath, Interface, QStringLiteral("SyncNow"));
                message << path;
                QDBusConnection::sessionBus().asyncCall(message);
            });
            result.append(syncNow);
        }
        if (const std::optional<QString> folder = addSyncTarget(cache, items)) {
            auto *addSync = new QAction(QStringLiteral("Sync with Google Drive…"), parentWidget);
            connect(addSync, &QAction::triggered, addSync, [path = *folder] {
                QProcess::startDetached(QStringLiteral("flatpak"), {QStringLiteral("run"), AppId, QStringLiteral("add-sync"), path});
            });
            result.append(addSync);
        }
        return result;
    }
};

K_PLUGIN_CLASS_WITH_JSON(SyncfastActions, "syncfastactions.json")

#include "actions.moc"

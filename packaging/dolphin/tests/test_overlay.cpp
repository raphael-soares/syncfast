#include "overlay.h"

#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QDBusVirtualObject>
#include <QSignalSpy>
#include <QTest>

#include <algorithm>
#include <memory>

using namespace syncfast;

struct RootRow {
    QString path;
    QString state;
};
Q_DECLARE_METATYPE(RootRow)

struct EntryRow {
    QString name;
    QString state;
    QString detail;
};
Q_DECLARE_METATYPE(EntryRow)

QDBusArgument &operator<<(QDBusArgument &arg, const RootRow &row)
{
    arg.beginStructure();
    arg << row.path << row.state;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, RootRow &row)
{
    arg.beginStructure();
    arg >> row.path >> row.state;
    arg.endStructure();
    return arg;
}

QDBusArgument &operator<<(QDBusArgument &arg, const EntryRow &row)
{
    arg.beginStructure();
    arg << row.name << row.state << row.detail;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, EntryRow &row)
{
    arg.beginStructure();
    arg >> row.name >> row.state >> row.detail;
    arg.endStructure();
    return arg;
}

class FakeService : public QDBusVirtualObject
{
public:
    QList<RootRow> roots;
    QHash<QString, QList<EntryRow>> directories;

    bool handleMessage(const QDBusMessage &message, const QDBusConnection &connection) override
    {
        if (message.interface() != Interface) {
            return false;
        }
        QDBusMessage reply;
        if (message.member() == QLatin1String("Roots")) {
            reply = message.createReply(QVariant::fromValue(roots));
        } else if (message.member() == QLatin1String("Directory")) {
            reply = message.createReply(QVariant::fromValue(directories.value(message.arguments().value(0).toString())));
        } else if (message.member() == QLatin1String("SyncNow")) {
            reply = message.createReply(true);
        } else {
            return false;
        }
        return connection.send(reply);
    }

    QString introspect(const QString &) const override
    {
        return {};
    }
};

namespace
{

const QString Drive = QStringLiteral("/srv/sf/Drive");
const QString File = Drive + QStringLiteral("/a.txt");

bool announced(const QSignalSpy &spy, const QString &path, const QStringList &overlays)
{
    return std::any_of(spy.cbegin(), spy.cend(), [&](const QList<QVariant> &args) {
        return args.at(0).toUrl() == QUrl::fromLocalFile(path) && args.at(1).toStringList() == overlays;
    });
}

QStringList overlaysOf(SyncfastOverlay &plugin, const QString &path)
{
    return plugin.getOverlays(QUrl::fromLocalFile(path));
}

}

class TestOverlay : public QObject
{
    Q_OBJECT

    QDBusConnection m_bus = QDBusConnection::connectToBus(QDBusConnection::SessionBus, QStringLiteral("fake-syncfast"));
    FakeService m_service;
    std::unique_ptr<SyncfastOverlay> m_plugin;

    void emitChanged()
    {
        QVERIFY(m_bus.send(QDBusMessage::createSignal(ObjectPath, Interface, QStringLiteral("Changed"))));
    }

private Q_SLOTS:
    void initTestCase()
    {
        if (!QDBusConnection::sessionBus().isConnected()) {
            QSKIP("no session bus");
        }
        qDBusRegisterMetaType<RootRow>();
        qDBusRegisterMetaType<QList<RootRow>>();
        qDBusRegisterMetaType<EntryRow>();
        qDBusRegisterMetaType<QList<EntryRow>>();
        m_service.roots = {{Drive, QStringLiteral("synced")}};
        m_service.directories.insert(Drive, {{QStringLiteral("a.txt"), QStringLiteral("pending"), QString()}});
        QVERIFY(m_bus.registerVirtualObject(ObjectPath, &m_service));
        QVERIFY(m_bus.registerService(BusName));
    }

    void aRunningServiceMarksItsRoot()
    {
        m_plugin = std::make_unique<SyncfastOverlay>();
        QSignalSpy spy(m_plugin.get(), &KOverlayIconPlugin::overlaysChanged);

        QTRY_VERIFY(announced(spy, Drive, {QStringLiteral("vcs-normal")}));
        QCOMPARE(overlaysOf(*m_plugin, Drive), QStringList({QStringLiteral("vcs-normal")}));
    }

    void aFileInsideARootIsAskedForAndThenMarked()
    {
        QSignalSpy spy(m_plugin.get(), &KOverlayIconPlugin::overlaysChanged);

        QCOMPARE(overlaysOf(*m_plugin, File), QStringList());

        QTRY_VERIFY(announced(spy, File, {QStringLiteral("vcs-update-required")}));
        QCOMPARE(overlaysOf(*m_plugin, File), QStringList({QStringLiteral("vcs-update-required")}));
    }

    void aChangedFileIsMarkedAgain()
    {
        QSignalSpy spy(m_plugin.get(), &KOverlayIconPlugin::overlaysChanged);
        m_service.directories.insert(Drive, {{QStringLiteral("a.txt"), QStringLiteral("error"), QStringLiteral("boom")}});

        emitChanged();

        QTRY_VERIFY(announced(spy, File, {QStringLiteral("vcs-conflicting")}));
        QCOMPARE(overlaysOf(*m_plugin, File), QStringList({QStringLiteral("vcs-conflicting")}));
    }

    void aChangedRootIsMarkedAgain()
    {
        QSignalSpy spy(m_plugin.get(), &KOverlayIconPlugin::overlaysChanged);
        m_service.roots = {{Drive, QStringLiteral("attention")}};

        emitChanged();

        QTRY_VERIFY(announced(spy, Drive, {QStringLiteral("vcs-conflicting")}));
        QCOMPARE(overlaysOf(*m_plugin, Drive), QStringList({QStringLiteral("vcs-conflicting")}));
    }

    void aRemoteLocationIsNeverMarked()
    {
        QCOMPARE(m_plugin->getOverlays(QUrl(QStringLiteral("sftp://h/srv/sf/Drive"))), QStringList());
    }

    void theServiceGoingAwayClearsEveryMark()
    {
        QSignalSpy spy(m_plugin.get(), &KOverlayIconPlugin::overlaysChanged);

        QVERIFY(m_bus.unregisterService(BusName));

        QTRY_VERIFY(announced(spy, Drive, {}) && announced(spy, File, {}));
        QCOMPARE(overlaysOf(*m_plugin, Drive), QStringList());
        QCOMPARE(overlaysOf(*m_plugin, File), QStringList());
    }

    void cleanupTestCase()
    {
        m_plugin.reset();
        m_bus.unregisterObject(ObjectPath);
        QDBusConnection::disconnectFromBus(QStringLiteral("fake-syncfast"));
    }
};

QTEST_GUILESS_MAIN(TestOverlay)

#include "test_overlay.moc"

#include "status.h"

#include <QTest>

using namespace syncfast;

namespace
{

const Pairs Roots = {{QStringLiteral("/home/u/Drive"), QStringLiteral("synced")}, {QStringLiteral("/home/u/Notes/"), QStringLiteral("syncing")}};

StatusCache cache(const Pairs &roots = Roots)
{
    return StatusCache(roots, true);
}

Selected folder(const QString &path)
{
    return {path, true};
}

}

class TestStatus : public QObject
{
    Q_OBJECT

private Q_SLOTS:
    void insideComparesWholePathComponents()
    {
        QVERIFY(inside(QStringLiteral("/a/Drive"), QStringLiteral("/a/Drive")));
        QVERIFY(inside(QStringLiteral("/a/Drive"), QStringLiteral("/a/Drive/x/y.txt")));
        QVERIFY(inside(QStringLiteral("/a/Drive/"), QStringLiteral("/a/Drive/x")));
        QVERIFY(!inside(QStringLiteral("/a/Drive"), QStringLiteral("/a/Drive2")));
        QVERIFY(!inside(QStringLiteral("/a/Drive"), QStringLiteral("/a/Drive2/x")));
        QVERIFY(!inside(QStringLiteral("/a/Drive"), QStringLiteral("/a")));
    }

    void rootIconForEachState_data()
    {
        QTest::addColumn<QString>("state");
        QTest::addColumn<QString>("icon");
        QTest::newRow("synced") << QStringLiteral("synced") << QStringLiteral("vcs-normal");
        QTest::newRow("pending") << QStringLiteral("pending") << QStringLiteral("vcs-update-required");
        QTest::newRow("syncing") << QStringLiteral("syncing") << QStringLiteral("vcs-update-required");
        QTest::newRow("conflict") << QStringLiteral("conflict") << QStringLiteral("vcs-conflicting");
        QTest::newRow("attention") << QStringLiteral("attention") << QStringLiteral("vcs-conflicting");
        QTest::newRow("unknown") << QStringLiteral("error") << QString();
    }

    void rootIconForEachState()
    {
        QFETCH(QString, state);
        QFETCH(QString, icon);
        const StatusCache status = cache({{QStringLiteral("/r"), state}});

        QCOMPARE(rootIcon(status.rootState(QStringLiteral("/r"))), icon);
    }

    void entryIconForEachState_data()
    {
        QTest::addColumn<QString>("state");
        QTest::addColumn<QString>("icon");
        QTest::newRow("synced") << QStringLiteral("synced") << QStringLiteral("vcs-normal");
        QTest::newRow("pending") << QStringLiteral("pending") << QStringLiteral("vcs-update-required");
        QTest::newRow("syncing") << QStringLiteral("syncing") << QStringLiteral("vcs-update-required");
        QTest::newRow("error") << QStringLiteral("error") << QStringLiteral("vcs-conflicting");
        QTest::newRow("conflict") << QStringLiteral("conflict") << QStringLiteral("vcs-conflicting");
        QTest::newRow("unknown") << QStringLiteral("attention") << QString();
    }

    void entryIconForEachState()
    {
        QFETCH(QString, state);
        QFETCH(QString, icon);

        QCOMPARE(entryIcon(state), icon);
    }

    void onlyTheRootHasARootState()
    {
        const StatusCache status = cache();

        QCOMPARE(status.rootState(QStringLiteral("/home/u/Notes")), QStringLiteral("syncing"));
        QCOMPARE(status.rootState(QStringLiteral("/home/u/Drive/a.txt")), QString());
        QCOMPARE(status.rootState(QStringLiteral("/home/u/Drive2")), QString());
    }

    void withoutTheServiceThereIsNoStateAndNoMenuTarget()
    {
        const StatusCache gone;

        QCOMPARE(gone.rootState(QStringLiteral("/home/u/Drive")), QString());
        QCOMPARE(gone.rootOf(QStringLiteral("/home/u/Drive/a")), std::nullopt);
        QCOMPARE(gone.syncTarget({QStringLiteral("/home/u/Drive/a")}), std::nullopt);
    }

    void nestedRootsPickTheInnermost()
    {
        const StatusCache status = cache({{QStringLiteral("/s"), QStringLiteral("synced")}, {QStringLiteral("/s/work"), QStringLiteral("synced")}});

        QCOMPARE(status.rootOf(QStringLiteral("/s/work/a")), QStringLiteral("/s/work"));
        QCOMPARE(status.rootOf(QStringLiteral("/s/other")), QStringLiteral("/s"));
    }

    void syncTargetIsTheFirstSelectedPathInsideARoot()
    {
        const StatusCache status = cache();

        QCOMPARE(status.syncTarget({QStringLiteral("/tmp/x"), QStringLiteral("/home/u/Notes/n.md"), QStringLiteral("/home/u/Drive/a")}),
                 QStringLiteral("/home/u/Notes/n.md"));
        QCOMPARE(status.syncTarget({QStringLiteral("/home/u/Drive")}), QStringLiteral("/home/u/Drive"));
        QCOMPARE(status.syncTarget({QStringLiteral("/home/u/Drive2/a")}), std::nullopt);
        QCOMPARE(status.syncTarget({}), std::nullopt);
    }

    void oneFolderOutsideEverySyncIsOffered()
    {
        QCOMPARE(addSyncTarget(cache(), {folder(QStringLiteral("/home/u/Photos"))}), QStringLiteral("/home/u/Photos"));
    }

    void aFolderInsideASyncOrHoldingOneIsNotOffered_data()
    {
        QTest::addColumn<QString>("path");
        QTest::newRow("root") << QStringLiteral("/home/u/Drive");
        QTest::newRow("inside") << QStringLiteral("/home/u/Drive/sub");
        QTest::newRow("holding") << QStringLiteral("/home/u");
        QTest::newRow("trailing slash root") << QStringLiteral("/home/u/Notes");
    }

    void aFolderInsideASyncOrHoldingOneIsNotOffered()
    {
        QFETCH(QString, path);

        QCOMPARE(addSyncTarget(cache(), {folder(path)}), std::nullopt);
    }

    void aFileTwoItemsOrARemoteLocationAreNotOffered()
    {
        QCOMPARE(addSyncTarget(cache(), {{QStringLiteral("/home/u/a.txt"), false}}), std::nullopt);
        QCOMPARE(addSyncTarget(cache(), {folder(QStringLiteral("/home/u/A")), folder(QStringLiteral("/home/u/B"))}), std::nullopt);
        QCOMPARE(addSyncTarget(cache(), {folder(QString())}), std::nullopt);
    }

    void withoutTheServiceAFolderIsStillOffered()
    {
        QCOMPARE(addSyncTarget(StatusCache(), {folder(QStringLiteral("/home/u/Drive"))}), QStringLiteral("/home/u/Drive"));
    }

    void changedPathsListsOnlyRootsWhoseStateChanged()
    {
        const StatusCache old = cache({{QStringLiteral("/r"), QStringLiteral("synced")}, {QStringLiteral("/q"), QStringLiteral("synced")}});
        const StatusCache now = cache({{QStringLiteral("/r"), QStringLiteral("syncing")},
                                       {QStringLiteral("/q"), QStringLiteral("synced")},
                                       {QStringLiteral("/n"), QStringLiteral("synced")}});

        QCOMPARE(changedPaths(old, now), QStringList({QStringLiteral("/n"), QStringLiteral("/r")}));
    }

    void theServiceVanishingChangesEveryRoot()
    {
        const StatusCache old = cache({{QStringLiteral("/r"), QStringLiteral("synced")}});

        QCOMPARE(changedPaths(old, StatusCache()), QStringList({QStringLiteral("/r")}));
    }

    void storeReturnsOnlyTheEntriesThatChanged()
    {
        DirectoryCache folders;
        const QStringList first = folders.store(
            QStringLiteral("/r/d"),
            {{QStringLiteral("a"), QStringLiteral("synced")}, {QStringLiteral("b"), QStringLiteral("pending")}, {QStringLiteral("c"), QStringLiteral("synced")}});

        const QStringList again = folders.store(
            QStringLiteral("/r/d"),
            {{QStringLiteral("a"), QStringLiteral("syncing")}, {QStringLiteral("b"), QStringLiteral("pending")}, {QStringLiteral("n"), QStringLiteral("error")}});

        QCOMPARE(first, QStringList({QStringLiteral("/r/d/a"), QStringLiteral("/r/d/b"), QStringLiteral("/r/d/c")}));
        QCOMPARE(again, QStringList({QStringLiteral("/r/d/a"), QStringLiteral("/r/d/c"), QStringLiteral("/r/d/n")}));
        QCOMPARE(folders.entry(QStringLiteral("/r/d/n")), QStringLiteral("error"));
        QCOMPARE(folders.entry(QStringLiteral("/r/d/c")), std::nullopt);
        QCOMPARE(folders.entry(QStringLiteral("/r/other/a")), std::nullopt);
    }

    void entriesOfTheFilesystemRootJoinWithOneSlash()
    {
        DirectoryCache folders;

        QCOMPARE(folders.store(QStringLiteral("/"), {{QStringLiteral("a"), QStringLiteral("synced")}}), QStringList({QStringLiteral("/a")}));
        QCOMPARE(folders.entry(QStringLiteral("/a")), QStringLiteral("synced"));
    }

    void anEmptyAnswerStillMakesTheFolderKnown()
    {
        DirectoryCache folders;

        folders.store(QStringLiteral("/r/empty"), {});

        QVERIFY(folders.known(QStringLiteral("/r/empty")));
        QVERIFY(!folders.known(QStringLiteral("/r")));
    }

    void pastTheLimitTheLeastRecentlyUsedFolderGoes()
    {
        DirectoryCache folders;
        for (int n = 0; n < MaxDirectories; ++n) {
            folders.store(QStringLiteral("/r/%1").arg(n), {{QStringLiteral("f"), QStringLiteral("synced")}});
        }
        folders.entry(QStringLiteral("/r/0/f"));

        folders.store(QStringLiteral("/r/new"), {});

        QVERIFY(folders.known(QStringLiteral("/r/0")));
        QVERIFY(!folders.known(QStringLiteral("/r/1")));
        QVERIFY(folders.known(QStringLiteral("/r/new")));
        QCOMPARE(folders.directories().size(), MaxDirectories);
    }

    void dropAllReturnsEveryPathWithAStateAndEmpties()
    {
        DirectoryCache folders;
        folders.store(QStringLiteral("/r"), {{QStringLiteral("a"), QStringLiteral("synced")}});
        folders.store(QStringLiteral("/r/d"), {{QStringLiteral("b"), QStringLiteral("pending")}});

        QCOMPARE(folders.dropAll(), QStringList({QStringLiteral("/r/a"), QStringLiteral("/r/d/b")}));
        QCOMPARE(folders.directories(), QStringList());
        QVERIFY(!folders.known(QStringLiteral("/r")));
    }
};

QTEST_GUILESS_MAIN(TestStatus)

#include "test_status.moc"

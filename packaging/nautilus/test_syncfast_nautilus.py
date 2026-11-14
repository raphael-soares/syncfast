import os
import sys
import unittest
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from syncfast_nautilus import (  # noqa: E402
    MAX_DIRECTORIES,
    DirectoryCache,
    StatusCache,
    ago,
    changed_paths,
    column_text,
    inside,
)

ROOTS = [("/home/u/Drive", "synced"), ("/home/u/Notes/", "syncing")]


def cache(roots=ROOTS):
    return StatusCache(roots, available=True)


class InsideTest(unittest.TestCase):
    def test_compares_whole_path_components(self):
        self.assertTrue(inside("/a/Drive", "/a/Drive"))
        self.assertTrue(inside("/a/Drive", "/a/Drive/x/y.txt"))
        self.assertTrue(inside("/a/Drive/", "/a/Drive/x"))
        self.assertFalse(inside("/a/Drive", "/a/Drive2"))
        self.assertFalse(inside("/a/Drive", "/a/Drive2/x"))
        self.assertFalse(inside("/a/Drive", "/a"))


class EmblemTest(unittest.TestCase):
    def test_each_root_state_has_its_emblem(self):
        states = {
            "synced": "emblem-default",
            "pending": "emblem-synchronizing",
            "syncing": "emblem-synchronizing",
            "conflict": "emblem-important",
            "attention": "emblem-important",
        }
        for state, emblem in states.items():
            with self.subTest(state=state):
                found = cache([("/r", state)]).emblem("/r")
                self.assertEqual(found, emblem)

    def test_only_the_root_gets_a_root_emblem(self):
        status = cache()

        self.assertEqual(status.emblem("/home/u/Drive"), "emblem-default")
        self.assertEqual(status.emblem("/home/u/Notes"), "emblem-synchronizing")
        self.assertIsNone(status.emblem("/home/u/Drive/a.txt"))
        self.assertIsNone(status.emblem("/home/u/Drive2"))

    def test_each_root_state_has_its_column_text(self):
        status = cache([("/r", "attention"), ("/s", "pending")])

        self.assertEqual(status.root_text("/r"), "Needs attention")
        self.assertEqual(status.root_text("/s"), "Waiting")
        self.assertIsNone(status.root_text("/r/a"))

    def test_without_the_service_there_is_no_emblem_and_no_menu_target(self):
        gone = StatusCache()

        self.assertIsNone(gone.emblem("/home/u/Drive"))
        self.assertIsNone(gone.sync_target(["/home/u/Drive/a"]))


class SyncTargetTest(unittest.TestCase):
    def test_first_selected_path_inside_a_root(self):
        status = cache()

        self.assertEqual(
            status.sync_target(["/tmp/x", "/home/u/Notes/n.md", "/home/u/Drive/a"]),
            "/home/u/Notes/n.md",
        )

    def test_the_root_itself_counts_and_a_sibling_with_a_shared_prefix_does_not(self):
        status = cache()

        self.assertEqual(status.sync_target(["/home/u/Drive"]), "/home/u/Drive")
        self.assertIsNone(status.sync_target(["/home/u/Drive2/a"]))
        self.assertIsNone(status.sync_target([]))

    def test_nested_roots_pick_the_innermost(self):
        status = cache([("/s", "synced"), ("/s/work", "synced")])

        self.assertEqual(status.root_of("/s/work/a"), "/s/work")
        self.assertEqual(status.root_of("/s/other"), "/s")


class ChangedPathsTest(unittest.TestCase):
    def test_lists_only_roots_whose_state_changed(self):
        old = cache([("/r", "synced"), ("/q", "synced")])
        new = cache([("/r", "syncing"), ("/q", "synced"), ("/n", "synced")])

        self.assertEqual(changed_paths(old, new), ["/n", "/r"])

    def test_the_service_vanishing_clears_every_emblem(self):
        old = cache([("/r", "synced")])

        self.assertEqual(changed_paths(old, StatusCache()), ["/r"])


class DirectoryCacheTest(unittest.TestCase):
    def test_store_returns_only_the_entries_that_changed(self):
        folders = DirectoryCache()
        first = folders.store(
            "/r/d", [("a", "synced", "t1"), ("b", "pending", ""), ("c", "synced", "t1")]
        )

        again = folders.store(
            "/r/d", [("a", "synced", "t2"), ("b", "pending", ""), ("n", "error", "x")]
        )

        self.assertEqual(first, ["/r/d/a", "/r/d/b", "/r/d/c"])
        self.assertEqual(again, ["/r/d/a", "/r/d/c", "/r/d/n"])
        self.assertEqual(folders.entry("/r/d/n"), ("error", "x"))
        self.assertIsNone(folders.entry("/r/d/c"))
        self.assertIsNone(folders.entry("/r/other/a"))

    def test_an_empty_answer_still_makes_the_folder_known(self):
        folders = DirectoryCache()

        folders.store("/r/empty", [])

        self.assertTrue(folders.known("/r/empty"))
        self.assertFalse(folders.known("/r"))

    def test_past_the_limit_the_least_recently_used_folder_goes(self):
        folders = DirectoryCache()
        for n in range(MAX_DIRECTORIES):
            folders.store(f"/r/{n}", [("f", "synced", "")])
        folders.entry("/r/0/f")

        folders.store("/r/new", [])

        self.assertTrue(folders.known("/r/0"))
        self.assertFalse(folders.known("/r/1"))
        self.assertTrue(folders.known("/r/new"))
        self.assertEqual(len(folders.directories()), MAX_DIRECTORIES)

    def test_drop_all_returns_every_path_with_a_state(self):
        folders = DirectoryCache()
        folders.store("/r", [("a", "synced", "")])
        folders.store("/r/d", [("b", "pending", "")])

        self.assertEqual(folders.drop_all(), ["/r/a", "/r/d/b"])
        self.assertEqual(folders.directories(), [])


class ColumnTextTest(unittest.TestCase):
    SYNCED_AT = datetime(2027, 1, 15, 8, tzinfo=timezone.utc).timestamp()

    def test_each_state_reads_as_text(self):
        cases = {
            ("pending", ""): "Waiting",
            ("syncing", ""): "Syncing",
            ("conflict", ""): "Conflict",
            ("error", "permission denied"): "Failed: permission denied",
            ("synced", "2027-01-15T08:00:00Z"): "Synced 2 h ago",
        }
        for (state, detail), text in cases.items():
            with self.subTest(state=state):
                now = self.SYNCED_AT + 2 * 3600
                self.assertEqual(column_text(state, detail, now), text)

    def test_synced_reads_rfc_3339_with_nanoseconds(self):
        detail = "2027-01-15T08:00:00.123456789Z"

        self.assertEqual(
            column_text("synced", detail, self.SYNCED_AT + 330), "Synced 5 min ago"
        )
        self.assertEqual(column_text("synced", "not a time", self.SYNCED_AT), "Synced")

    def test_ago_boundaries(self):
        self.assertEqual(ago(-5), "just now")
        self.assertEqual(ago(59), "just now")
        self.assertEqual(ago(60), "1 min ago")
        self.assertEqual(ago(3599), "59 min ago")
        self.assertEqual(ago(3600), "1 h ago")
        self.assertEqual(ago(86399), "23 h ago")
        self.assertEqual(ago(86400), "1 d ago")


if __name__ == "__main__":
    unittest.main()

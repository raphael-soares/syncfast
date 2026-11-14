import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from syncfast_nautilus import StatusCache, changed_paths, inside  # noqa: E402

ROOTS = [("/home/u/Drive", "synced"), ("/home/u/Notes/", "syncing")]


def cache(roots=ROOTS, conflicts=()):
    return StatusCache(roots, conflicts, available=True)


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

    def test_only_the_root_and_conflicted_files_get_an_emblem(self):
        status = cache(conflicts=["/home/u/Drive/a.txt.conflict1"])

        self.assertEqual(status.emblem("/home/u/Drive"), "emblem-default")
        self.assertEqual(status.emblem("/home/u/Notes"), "emblem-synchronizing")
        self.assertEqual(
            status.emblem("/home/u/Drive/a.txt.conflict1"), "emblem-important"
        )
        self.assertIsNone(status.emblem("/home/u/Drive/a.txt"))
        self.assertIsNone(status.emblem("/home/u/Drive2"))

    def test_a_conflicted_file_wins_over_a_synced_root_state(self):
        status = cache([("/r", "synced")], conflicts=["/r"])

        self.assertEqual(status.emblem("/r"), "emblem-important")

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
    def test_lists_only_paths_whose_emblem_changed(self):
        old = cache([("/r", "synced"), ("/q", "synced")])
        new = cache([("/r", "syncing"), ("/q", "synced")], conflicts=["/r/f.conflict1"])

        changed = changed_paths(old, new)

        self.assertEqual(changed, ["/r", "/r/f.conflict1"])

    def test_a_conflict_that_was_settled_loses_its_emblem(self):
        old = cache([("/r", "conflict")], conflicts=["/r/f.conflict1"])
        new = cache([("/r", "synced")])

        self.assertEqual(changed_paths(old, new), ["/r", "/r/f.conflict1"])

    def test_the_service_vanishing_clears_every_emblem(self):
        old = cache([("/r", "synced")])

        self.assertEqual(changed_paths(old, StatusCache()), ["/r"])


if __name__ == "__main__":
    unittest.main()

"""Nautilus emblems and "Sync Now" for syncfast.

Reads the status service that `syncfast watch` publishes on the session bus
(io.github.raphael_soares.syncfast.Status). Without the service there are
no emblems and no menu. The D-Bus calls are asynchronous and the file
callbacks only read a cache.
"""

from typing import Dict, FrozenSet, Iterable, List, Optional, Sequence, Tuple

BUS_NAME = "io.github.raphael_soares.syncfast.Status"
OBJECT_PATH = "/io/github/raphael_soares/syncfast/Status"
INTERFACE = BUS_NAME

EMBLEMS = {
    "synced": "emblem-default",
    "pending": "emblem-synchronizing",
    "syncing": "emblem-synchronizing",
    "conflict": "emblem-important",
    "attention": "emblem-important",
}
CONFLICTED_FILE = "emblem-important"


def inside(root: str, path: str) -> bool:
    root = root.rstrip("/")
    return path == root or path.startswith(root + "/")


class StatusCache:
    """What the service last answered. `available` is False without it."""

    def __init__(
        self,
        roots: Sequence[Tuple[str, str]] = (),
        conflicts: Iterable[str] = (),
        available: bool = False,
    ) -> None:
        self.roots: Dict[str, str] = {
            path.rstrip("/") or "/": state for path, state in roots
        }
        self.conflicts: FrozenSet[str] = frozenset(conflicts)
        self.available = available

    def root_of(self, path: str) -> Optional[str]:
        held = [root for root in self.roots if inside(root, path)]
        return max(held, key=len) if held else None

    def emblem(self, path: str) -> Optional[str]:
        if not self.available:
            return None
        if path in self.conflicts:
            return CONFLICTED_FILE
        state = self.roots.get(path)
        return EMBLEMS.get(state) if state is not None else None

    def sync_target(self, paths: Iterable[str]) -> Optional[str]:
        if not self.available:
            return None
        return next((path for path in paths if self.root_of(path)), None)


def changed_paths(old: StatusCache, new: StatusCache) -> List[str]:
    candidates = set(old.roots) | set(new.roots) | old.conflicts | new.conflicts
    return sorted(path for path in candidates if old.emblem(path) != new.emblem(path))


def _require_nautilus(gi) -> None:
    """Older Nautilus ships the 4.0 typelib; Nautilus 51 ships only 4.1."""
    for version in ("4.1", "4.0"):
        try:
            gi.require_version("Nautilus", version)
            return
        except ValueError:
            continue
    raise ValueError("no Nautilus 4 typelib")


try:
    import gi

    _require_nautilus(gi)
    from gi.repository import Gio, GLib, GObject, Nautilus
except (ImportError, ValueError):
    Nautilus = None


def _local_path(file_info) -> Optional[str]:
    if file_info.get_uri_scheme() != "file":
        return None
    return file_info.get_location().get_path()


if Nautilus is not None:

    class SyncfastExtension(
        GObject.GObject, Nautilus.InfoProvider, Nautilus.MenuProvider
    ):
        def __init__(self) -> None:
            super().__init__()
            self._cache = StatusCache()
            self._connection = None
            self._subscription = None
            self._generation = 0
            Gio.bus_watch_name(
                Gio.BusType.SESSION,
                BUS_NAME,
                Gio.BusNameWatcherFlags.NONE,
                self._on_appeared,
                self._on_vanished,
            )

        def update_file_info(self, file) -> None:
            path = _local_path(file)
            if path is None:
                return
            emblem = self._cache.emblem(path)
            if emblem:
                file.add_emblem(emblem)

        def get_file_items(self, files):
            paths = [p for p in map(_local_path, files) if p]
            return self._menu(self._cache.sync_target(paths))

        def get_background_items(self, folder):
            path = _local_path(folder)
            return self._menu(self._cache.sync_target([path] if path else []))

        def _menu(self, target: Optional[str]):
            if target is None:
                return []
            item = Nautilus.MenuItem(
                name="SyncfastExtension::sync_now", label="Sync Now"
            )
            item.connect("activate", self._on_sync_now, target)
            return [item]

        def _on_sync_now(self, _item, path: str) -> None:
            if self._connection is None:
                return
            self._connection.call(
                BUS_NAME,
                OBJECT_PATH,
                INTERFACE,
                "SyncNow",
                GLib.Variant("(s)", (path,)),
                GLib.VariantType("(b)"),
                Gio.DBusCallFlags.NONE,
                -1,
                None,
                None,
            )

        def _on_appeared(self, connection, _name, _owner) -> None:
            self._connection = connection
            self._subscription = connection.signal_subscribe(
                BUS_NAME,
                INTERFACE,
                "Changed",
                OBJECT_PATH,
                None,
                Gio.DBusSignalFlags.NONE,
                lambda *_args: self._refresh(),
            )
            self._refresh()

        def _on_vanished(self, connection, _name) -> None:
            if self._connection is not None and self._subscription is not None:
                self._connection.signal_unsubscribe(self._subscription)
            self._connection = None
            self._subscription = None
            self._generation += 1
            self._replace(StatusCache())

        def _call(self, method: str, reply_type: str, callback) -> None:
            self._connection.call(
                BUS_NAME,
                OBJECT_PATH,
                INTERFACE,
                method,
                None,
                GLib.VariantType(reply_type),
                Gio.DBusCallFlags.NONE,
                -1,
                None,
                callback,
            )

        def _refresh(self) -> None:
            if self._connection is None:
                return
            self._generation += 1
            generation = self._generation
            connection = self._connection

            def got_conflicts(_source, result, roots) -> None:
                if generation != self._generation:
                    return
                try:
                    (conflicts,) = connection.call_finish(result).unpack()
                except GLib.Error:
                    self._replace(StatusCache())
                    return
                self._replace(StatusCache(roots, conflicts, available=True))

            def got_roots(_source, result) -> None:
                if generation != self._generation:
                    return
                try:
                    (roots,) = connection.call_finish(result).unpack()
                except GLib.Error:
                    self._replace(StatusCache())
                    return
                self._call(
                    "Conflicts",
                    "(as)",
                    lambda source, res: got_conflicts(source, res, roots),
                )

            self._call("Roots", "(a(ss))", got_roots)

        def _replace(self, new: StatusCache) -> None:
            old, self._cache = self._cache, new
            for path in changed_paths(old, new):
                shown = Nautilus.FileInfo.lookup(Gio.File.new_for_path(path))
                if shown is not None:
                    shown.invalidate_extension_info()

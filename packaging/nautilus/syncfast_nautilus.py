"""Nautilus emblems, a "Sync" column and "Sync Now" for syncfast.

Reads the status service that `syncfast watch` publishes on the session bus
(io.github.raphael_soares.syncfast.Status). Every file and folder inside a
sync gets an emblem for its state (synced, waiting, syncing, failed or in
conflict), and the list view can show the same state as text in the "Sync"
column. What the filters leave out gets neither. Without the service there
are no emblems and no menu. The D-Bus calls are asynchronous and the file
callbacks only read a cache.
"""

import os
from datetime import datetime
from typing import Dict, Iterable, List, Optional, Sequence, Tuple

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
ROOT_TEXT = {
    "synced": "Synced",
    "pending": "Waiting",
    "syncing": "Syncing",
    "conflict": "Conflict",
    "attention": "Needs attention",
}
ENTRY_EMBLEMS = {
    "synced": "emblem-default",
    "pending": "emblem-synchronizing",
    "syncing": "emblem-synchronizing",
    "error": "emblem-important",
    "conflict": "emblem-important",
}
STATUS_ATTRIBUTE = "syncfast_status"
MAX_DIRECTORIES = 64


def inside(root: str, path: str) -> bool:
    root = root.rstrip("/")
    return path == root or path.startswith(root + "/")


class StatusCache:
    """What `Roots` last answered. `available` is False without the service."""

    def __init__(
        self,
        roots: Sequence[Tuple[str, str]] = (),
        available: bool = False,
    ) -> None:
        self.roots: Dict[str, str] = {
            path.rstrip("/") or "/": state for path, state in roots
        }
        self.available = available

    def root_of(self, path: str) -> Optional[str]:
        if not self.available:
            return None
        held = [root for root in self.roots if inside(root, path)]
        return max(held, key=len) if held else None

    def root_state(self, path: str) -> Optional[str]:
        return self.roots.get(path) if self.available else None

    def emblem(self, path: str) -> Optional[str]:
        return EMBLEMS.get(self.root_state(path) or "")

    def root_text(self, path: str) -> Optional[str]:
        return ROOT_TEXT.get(self.root_state(path) or "")

    def sync_target(self, paths: Iterable[str]) -> Optional[str]:
        return next((path for path in paths if self.root_of(path)), None)


def changed_paths(old: StatusCache, new: StatusCache) -> List[str]:
    candidates = set(old.roots) | set(new.roots)
    return sorted(p for p in candidates if old.root_state(p) != new.root_state(p))


class DirectoryCache:
    """What `Directory` last answered, for the folders most recently used."""

    def __init__(self) -> None:
        self._folders: Dict[str, Dict[str, Tuple[str, str]]] = {}

    def store(self, directory: str, entries: Sequence[Tuple[str, str, str]]) -> List[str]:
        """Keeps the answer; the paths whose state or detail changed."""
        new = {name: (state, detail) for name, state, detail in entries}
        old = self._folders.pop(directory, {})
        self._folders[directory] = new
        while len(self._folders) > MAX_DIRECTORIES:
            del self._folders[next(iter(self._folders))]
        names = set(old) | set(new)
        return sorted(
            os.path.join(directory, name)
            for name in names
            if old.get(name) != new.get(name)
        )

    def entry(self, path: str) -> Optional[Tuple[str, str]]:
        directory, name = os.path.split(path)
        entries = self._folders.pop(directory, None)
        if entries is None:
            return None
        self._folders[directory] = entries
        return entries.get(name)

    def known(self, directory: str) -> bool:
        return directory in self._folders

    def directories(self) -> List[str]:
        return list(self._folders)

    def drop_all(self) -> List[str]:
        """Forgets everything; the paths that had a state."""
        paths = sorted(
            os.path.join(directory, name)
            for directory, entries in self._folders.items()
            for name in entries
        )
        self._folders.clear()
        return paths


def ago(seconds: float) -> str:
    if seconds < 60:
        return "just now"
    if seconds < 3600:
        return f"{int(seconds // 60)} min ago"
    if seconds < 86400:
        return f"{int(seconds // 3600)} h ago"
    return f"{int(seconds // 86400)} d ago"


def _parse_time(text: str) -> Optional[float]:
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        return datetime.fromisoformat(text).timestamp()
    except ValueError:
        return None


def column_text(state: str, detail: str, now: float) -> str:
    if state == "synced":
        at = _parse_time(detail)
        return "Synced" if at is None else "Synced " + ago(now - at)
    if state == "error":
        return "Failed: " + detail
    return {"pending": "Waiting", "syncing": "Syncing", "conflict": "Conflict"}.get(
        state, ""
    )


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
    import time

    _PROVIDERS = [GObject.GObject, Nautilus.InfoProvider, Nautilus.MenuProvider]
    if hasattr(Nautilus, "ColumnProvider"):
        _PROVIDERS.append(Nautilus.ColumnProvider)

    class SyncfastExtension(*_PROVIDERS):
        def __init__(self) -> None:
            super().__init__()
            self._cache = StatusCache()
            self._entries = DirectoryCache()
            self._inflight = set()
            self._stale = set()
            self._connection = None
            self._subscription = None
            self._generation = 0
            self._directory_generation = 0
            Gio.bus_watch_name(
                Gio.BusType.SESSION,
                BUS_NAME,
                Gio.BusNameWatcherFlags.NONE,
                self._on_appeared,
                self._on_vanished,
            )

        def get_columns(self):
            return [
                Nautilus.Column(
                    name="SyncfastExtension::status",
                    attribute=STATUS_ATTRIBUTE,
                    label="Sync",
                    description="Sync status from syncfast",
                )
            ]

        def update_file_info(self, file) -> None:
            path = _local_path(file)
            if path is None:
                return
            emblem = self._cache.emblem(path)
            if emblem:
                file.add_emblem(emblem)
                file.add_string_attribute(STATUS_ATTRIBUTE, self._cache.root_text(path))
                return
            if self._cache.root_of(path) is None:
                return
            found = self._entries.entry(path)
            if found is not None:
                state, detail = found
                if state in ENTRY_EMBLEMS:
                    file.add_emblem(ENTRY_EMBLEMS[state])
                file.add_string_attribute(
                    STATUS_ATTRIBUTE, column_text(state, detail, time.time())
                )
                return
            parent = os.path.dirname(path)
            if not self._entries.known(parent):
                self._request(parent)

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
                lambda *_args: self._on_changed(),
            )
            self._on_changed()

        def _on_vanished(self, connection, _name) -> None:
            if self._connection is not None and self._subscription is not None:
                self._connection.signal_unsubscribe(self._subscription)
            self._connection = None
            self._subscription = None
            self._generation += 1
            self._directory_generation += 1
            self._inflight.clear()
            self._stale.clear()
            self._replace(StatusCache())
            self._invalidate(self._entries.drop_all())

        def _on_changed(self) -> None:
            self._refresh()
            for directory in set(self._entries.directories()) | self._inflight:
                self._request(directory, again=True)

        def _refresh(self) -> None:
            if self._connection is None:
                return
            self._generation += 1
            generation = self._generation
            connection = self._connection

            def got_roots(_source, result) -> None:
                if generation != self._generation:
                    return
                try:
                    (roots,) = connection.call_finish(result).unpack()
                except GLib.Error:
                    self._replace(StatusCache())
                    return
                self._replace(StatusCache(roots, available=True))

            connection.call(
                BUS_NAME,
                OBJECT_PATH,
                INTERFACE,
                "Roots",
                None,
                GLib.VariantType("(a(ss))"),
                Gio.DBusCallFlags.NONE,
                -1,
                None,
                got_roots,
            )

        def _request(self, directory: str, again: bool = False) -> None:
            """Asks `Directory` for one folder at a time. `again` asks once
            more after an answer in flight, which may predate the change."""
            if self._connection is None:
                return
            if directory in self._inflight:
                if again:
                    self._stale.add(directory)
                return
            self._inflight.add(directory)
            generation = self._directory_generation
            connection = self._connection

            def got_directory(_source, result) -> None:
                if generation != self._directory_generation:
                    return
                self._inflight.discard(directory)
                try:
                    (entries,) = connection.call_finish(result).unpack()
                except GLib.Error:
                    entries = []
                self._invalidate(self._entries.store(directory, entries))
                if directory in self._stale:
                    self._stale.discard(directory)
                    self._request(directory)

            connection.call(
                BUS_NAME,
                OBJECT_PATH,
                INTERFACE,
                "Directory",
                GLib.Variant("(s)", (directory,)),
                GLib.VariantType("(a(sss))"),
                Gio.DBusCallFlags.NONE,
                -1,
                None,
                got_directory,
            )

        def _replace(self, new: StatusCache) -> None:
            old, self._cache = self._cache, new
            self._invalidate(changed_paths(old, new))

        def _invalidate(self, paths: Iterable[str]) -> None:
            for path in paths:
                shown = Nautilus.FileInfo.lookup(Gio.File.new_for_path(path))
                if shown is not None:
                    shown.invalidate_extension_info()

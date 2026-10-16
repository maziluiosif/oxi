"""oxi chat inside Sublime Text.

Three views make up the chat: the chat list in a column on the left, the transcript of the
open chat on its right and the composer under the transcript. All the agent work happens in `oxi headless`, a subprocess this
plugin starts on first use; see `lib/client.py` for the protocol and `src/headless/` in the
oxi repository for the other side.
"""

import base64
import hashlib
import html
import importlib
import json
import os
import shutil
import sys
import tempfile
import time

import sublime
import sublime_plugin

# Sublime reloads this file when it changes but not the modules it imports: reload them first.
for _name in ("render", "scheme", "client"):
    _module = sys.modules.get("{}.lib.{}".format(__package__, _name))
    if _module is not None:
        importlib.reload(_module)

from .lib import render, scheme  # noqa: E402
from .lib.client import Client  # noqa: E402

PACKAGE = __package__ or "Oxi"
SETTINGS_FILE = "Oxi.sublime-settings"
TRANSCRIPT_SYNTAX = "Packages/{}/OxiTranscript.sublime-syntax".format(PACKAGE)
CHATS_SYNTAX = "Packages/{}/OxiChats.sublime-syntax".format(PACKAGE)
COMPOSER_SYNTAX = "Packages/Markdown/Markdown.sublime-syntax"
PROTOCOL = 1
EFFORTS = ["", "low", "medium", "high", "xhigh", "max"]
IGNORED_DIRS = {
    ".git", ".hg", ".svn", "node_modules", "target", "dist", "build", ".venv", "venv",
    "__pycache__", ".idea", ".next", ".cache", ".tox", ".mypy_cache", ".pytest_cache",
}
MAX_MENTION_FILES = 20000
# The same limits as the oxi app (src/theme.rs).
MAX_IMAGES = 8
MAX_IMAGE_BYTES = 6 * 1024 * 1024
IMAGE_TYPES = {"png": "image/png", "jpg": "image/jpeg", "jpeg": "image/jpeg", "gif": "image/gif", "webp": "image/webp"}

CLIENT = None
READY = False
WAITING = []
PROVIDERS = []
PANELS = {}
LOG = []
FILE_CACHE = {}


def settings():
    return sublime.load_settings(SETTINGS_FILE)


def log(text):
    LOG.append(text)
    del LOG[:-1000]
    if settings().get("debug"):
        print("oxi:", text)


def find_binary():
    configured = (settings().get("oxi_path") or "").strip()
    if configured:
        path = os.path.expanduser(os.path.expandvars(configured))
        return path if os.path.isfile(path) else None
    candidates = [
        shutil.which("oxi"),
        "~/.local/bin/oxi",
        "/Applications/oxi.app/Contents/MacOS/oxi",
        "~/Applications/oxi.app/Contents/MacOS/oxi",
        "%LOCALAPPDATA%\\Programs\\oxi\\oxi.exe",
        "/usr/local/bin/oxi",
    ]
    for candidate in candidates:
        if not candidate:
            continue
        path = os.path.expanduser(os.path.expandvars(candidate))
        if os.path.isfile(path):
            return path
    return None


def with_client(then):
    """Run `then()` once oxi is up, starting it first if needed."""
    global CLIENT, READY
    if CLIENT is not None and CLIENT.alive() and READY:
        then()
        return
    WAITING.append(then)
    if CLIENT is not None and CLIENT.alive():
        return
    binary = find_binary()
    if not binary:
        WAITING.clear()
        sublime.error_message(
            "oxi was not found.\n\nInstall oxi or set \"oxi_path\" in "
            "Preferences → Package Settings → Oxi."
        )
        return
    READY = False
    try:
        client = Client(binary, lambda f: sublime.set_timeout(f, 0), on_notification, on_exit, log)
    except OSError as e:
        WAITING.clear()
        sublime.error_message("Could not start oxi ({}):\n{}".format(binary, e))
        return
    CLIENT = client
    log("started {} headless".format(binary))

    def on_initialize(result, error):
        global READY, PROVIDERS
        if CLIENT is not client:
            return
        if error or not isinstance(result, dict) or result.get("protocol") != PROTOCOL:
            WAITING.clear()
            client.proc.kill()
            sublime.error_message(
                "oxi ({}) did not start its editor server: {}".format(
                    binary, error or "unsupported protocol"
                )
            )
            return
        PROVIDERS[:] = result.get("providers") or []
        READY = True
        log("oxi {} ready".format(result.get("version")))
        callbacks = WAITING[:]
        WAITING.clear()
        for callback in callbacks:
            callback()

    def check_started():
        # A build without `oxi headless` opens its window instead of answering.
        if CLIENT is client and not READY:
            WAITING.clear()
            client.proc.kill()
            sublime.error_message(
                "oxi at {} did not answer.\n\nThe Sublime Text package needs an oxi build with "
                "the `oxi headless` command; set \"oxi_path\" to one.".format(binary)
            )

    client.request("initialize", {}, on_initialize)
    sublime.set_timeout(check_started, 8000)


def request(method, params, callback=None):
    """Send a request once oxi is up; errors without a callback go to the status bar."""

    def send():
        def done(result, error):
            if callback is not None:
                callback(result, error)
            elif error:
                sublime.status_message("oxi: " + error)

        CLIENT.request(method, params, done)

    with_client(send)


def on_exit():
    global CLIENT, READY
    if CLIENT is not None and CLIENT.alive():
        return
    CLIENT = None
    READY = False
    for panel in list(PANELS.values()):
        panel.running = False
        panel.approval = None
        panel.status = "oxi stopped; it restarts with the next message"
        panel.refresh_toolbar()
        panel.update_approval()


def on_notification(method, params):
    chat = params.get("chat")
    for panel in list(PANELS.values()):
        if method == "chats_changed":
            if same_path(params.get("root"), panel.root):
                panel.refresh_chats()
            continue
        if chat is None or chat != panel.chat_id or panel.loading:
            if method == "approval" and params.get("approval") and chat:
                sublime.status_message("oxi: a chat is waiting for your approval")
                panel.refresh_chats()
            continue
        # The other notifications come from any editor connected to oxi, this one included:
        # a chat open in two windows (or in Sublime and another editor) stays in step.
        if method == "message":
            panel.on_message(params["index"], params["message"])
        elif method == "turn":
            panel.on_turn(params)
        elif method == "approval":
            panel.approval = params.get("approval")
            panel.update_approval()
            panel.refresh_toolbar()
            if panel.approval:
                scroll_to_end(panel.views["transcript"])
                sublime.status_message("oxi: waiting for your approval")
        elif method == "status":
            panel.status = params.get("text")
            panel.refresh_toolbar()
        elif method == "context":
            panel.context = {"used": params.get("used"), "size": params.get("size")}
            panel.refresh_toolbar()
        elif method == "config":
            panel.config = params.get("config")
            panel.refresh_toolbar()
        elif method == "title":
            panel.set_title(params.get("title"))
        elif method == "chat_reloaded":
            # The app (or an older oxi) wrote the chat; read it again, keeping the scroll.
            panel.open_chat(chat, scroll=False)
        elif method == "chat_removed":
            panel.new_chat()


def same_path(a, b):
    if not a or not b:
        return False
    return comparable_path(a) == comparable_path(b)


def comparable_path(path):
    # oxi canonicalizes roots, which on Windows gives verbatim `\\?\C:\...` paths that
    # realpath keeps as they are, so strip the prefix and fold case before comparing.
    if path.startswith("\\\\?\\UNC\\"):
        path = "\\\\" + path[8:]
    elif path.startswith("\\\\?\\"):
        path = path[4:]
    return os.path.normcase(os.path.realpath(path))


def window_root(window):
    folders = window.folders()
    if folders:
        return os.path.realpath(folders[0])
    view = window.active_view()
    if view is not None and view.file_name():
        return os.path.realpath(os.path.dirname(view.file_name()))
    return None


def provider_label(slug):
    for provider in PROVIDERS:
        if provider.get("id") == slug:
            return provider.get("label") or slug
    return slug or "?"


def panel_for(window, create=False):
    if window is None:
        return None
    panel = PANELS.get(window.id())
    if panel is not None and panel.alive():
        return panel
    if panel is not None:
        panel.teardown()
    if not create:
        return None
    root = window_root(window)
    if root is None:
        sublime.error_message("Open a folder (or a file) first: oxi chats belong to a workspace.")
        return None
    panel = Panel(window, root)
    PANELS[window.id()] = panel
    panel.open()
    return panel


def view_panel(view):
    if view is None:
        return None
    window = view.window()
    if window is None:
        return None
    panel = PANELS.get(window.id())
    if panel is None or not panel.alive():
        return None
    return panel


def is_role(view, role):
    return view is not None and view.settings().get("oxi_role") == role


def at_bottom(view):
    _, y = view.viewport_position()
    _, height = view.viewport_extent()
    _, total = view.layout_extent()
    return y + height >= total - view.line_height() * 2


def scroll_to_end(view):
    def scroll():
        _, height = view.viewport_extent()
        _, total = view.layout_extent()
        view.set_viewport_position((0, max(0, total - height)), False)

    sublime.set_timeout(scroll, 0)


class Panel:
    """The oxi column of one window and the chat it shows."""

    def __init__(self, window, root):
        self.window = window
        self.root = root
        self.chat_id = None
        self.title = "New chat"
        self.messages = []
        self.starts = []
        self.links = []
        self.config = None
        self.running = False
        self.queued = 0
        self.plan = False
        self.approval = None
        self.status = None
        self.context = None
        # Usage windows of the chat's provider (`quota.get`), shown in the toolbar.
        self.quota = None
        self.quota_provider = None
        self.loading = False
        self.chats = []
        self.chat_rows = []
        self.views = {}
        self.history_upto = 0
        self.closing = False
        self.phantoms = {}
        # Shown as a red card at the end of the transcript until the next send.
        self.error = None
        # Images attached to the next message, `{"mime", "data"}` with the data in base64.
        self.images = []

    # ---- layout -------------------------------------------------------------------------

    def alive(self):
        return (
            not self.closing
            and all(role in self.views and self.views[role].is_valid() for role in ("transcript", "composer"))
        )

    def open(self):
        """Lay out the chat list on the left and the chat over the composer on its right.

        The oxi area goes to the right of the window's own groups, or takes the whole window when
        no file is open.
        """
        window = self.window
        s = settings()
        layout = window.get_layout()
        if not window.settings().get("oxi_saved_layout"):
            window.settings().set("oxi_saved_layout", layout)
            window.settings().set("oxi_saved_minimap", window.is_minimap_visible())
        # Sublime has no per-view minimap, and a narrow chat list has no use for one.
        if s.get("hide_minimap", True):
            window.set_minimap_visible(False)
        chats_width = min(0.4, max(0.08, float(s.get("chats_width", 0.18))))
        chat_width = min(0.8, max(0.2, float(s.get("panel_width", 0.42))))
        composer_height = min(0.5, max(0.08, float(s.get("composer_height", 0.22))))
        split = 1.0 - composer_height
        has_files = any(v.settings().get("oxi_role") is None for v in window.views())
        if has_files:
            share = min(0.9, chats_width + chat_width)
            cols = [c * (1.0 - share) for c in layout["cols"]]
            cols += [cols[-1] + (1.0 - cols[-1]) * chats_width / (chats_width + chat_width), 1.0]
            rows = sorted(set(layout["rows"] + [split]))
            index = {i: rows.index(r) for i, r in enumerate(layout["rows"])}
            cells = [[x1, index[y1], x2, index[y2]] for x1, y1, x2, y2 in layout["cells"]]
        else:
            cols = [0.0, chats_width, 1.0]
            rows = [0.0, split, 1.0]
            cells = []
        first = len(cells)
        col = len(cols) - 3
        middle, last = rows.index(split), len(rows) - 1
        cells += [
            [col, 0, col + 1, last],
            [col + 1, 0, col + 2, middle],
            [col + 1, middle, col + 2, last],
        ]
        window.set_layout({"cols": cols, "rows": rows, "cells": cells})
        self.chats_group = first
        self.ensure_chats_view()
        self.views["transcript"] = self.new_view(first + 1, "transcript", "oxi", TRANSCRIPT_SYNTAX, {
            "word_wrap": True,
            "wrap_width": 0,
            "highlight_line": False,
        })
        self.views["composer"] = self.new_view(first + 2, "composer", "✎ Message", COMPOSER_SYNTAX, {
            "word_wrap": True,
            "auto_complete": True,
            # Completions pop up only after "@" (file mentions), never while typing words.
            "auto_complete_triggers": [{"selector": "text", "characters": "@"}],
            "auto_complete_selector": "never.oxi",
            "spell_check": False,
            "highlight_line": False,
            "auto_indent": False,
        })
        self.views["composer"].set_read_only(False)
        update_transcript_colors(self.views["transcript"])
        for key in ("header", "history", "tail", "approval"):
            self.phantoms[key] = sublime.PhantomSet(self.views["transcript"], "oxi_" + key)
        self.phantoms["toolbar"] = sublime.PhantomSet(self.views["composer"], "oxi_toolbar")
        last_chat = window.settings().get("oxi_last_chat")
        self.new_chat()
        if last_chat and os.path.isfile(last_chat):
            self.open_chat(last_chat)
        self.refresh_chats()
        self.focus_composer()
        self.poll_quota()

    def poll_quota(self):
        if not self.alive():
            return
        self.refresh_quota()
        sublime.set_timeout(self.poll_quota, QUOTA_POLL_MS)

    def refresh_quota(self):
        provider = (self.config or {}).get("provider")
        self.quota_provider = provider
        if not provider:
            return

        def done(result, error):
            if self.quota_provider != provider or not self.alive():
                return
            self.quota = None if error else result
            self.refresh_toolbar()

        request("quota.get", {"provider": provider}, done)

    def quota_html(self):
        quota = self.quota or {}
        if quota.get("provider") != self.quota_provider or not quota.get("windows"):
            return ""
        parts = []
        for window in quota["windows"]:
            used = float(window.get("used_pct") or 0)
            text = "{} {:.0f}%".format(window.get("label") or "", used)
            if window.get("resets_in"):
                text += " ↻" + render.format_duration(window["resets_in"])
            level = "full" if used >= 90 else "high" if used >= 70 else "muted"
            parts.append('<span class="{}">{}</span>'.format(level, html.escape(text)))
        if quota.get("limit_reached"):
            parts.append('<span class="full">limit reached</span>')
        return " · ".join(parts)

    def editor_group(self):
        """A group for files opened from the chat, outside the oxi columns.

        The focused or first non-oxi group when there is one; otherwise a column is added on
        the right of the chat, so files never land among the chat list or the transcript.
        """
        window = self.window
        oxi_groups = {
            window.get_view_index(view)[0] for view in self.views.values() if view.is_valid()
        }
        others = [g for g in range(window.num_groups()) if g not in oxi_groups]
        if window.active_group() in others:
            return window.active_group()
        if others:
            return others[0]
        transcript_group = window.get_view_index(self.views["transcript"])[0]
        layout = window.get_layout()
        cells = layout["cells"]
        x1, _, x2, _ = cells[transcript_group]
        cols = list(layout["cols"])
        split = cols[x1] + (cols[x2] - cols[x1]) * 0.5
        cols.insert(x2, split)
        # The chat column now ends at the new split; whatever was right of it moves over one.
        shifted = []
        for cx1, cy1, cx2, cy2 in cells:
            shifted.append([
                cx1 + (1 if cx1 >= x2 else 0), cy1,
                cx2 + (1 if cx2 > x2 else 0), cy2,
            ])
        shifted.append([x2, 0, x2 + 1, len(layout["rows"]) - 1])
        window.set_layout({"cols": cols, "rows": layout["rows"], "cells": shifted})
        # Remembered so closing the last file there gives the chat its width back.
        self.files_group = len(shifted) - 1
        self.layout_before_files = layout
        return self.files_group

    def collapse_files(self):
        """Remove the files column added by `editor_group` once its last tab is closed."""
        group = getattr(self, "files_group", None)
        if group is None or not self.alive():
            return
        window = self.window
        if group >= window.num_groups():
            self.files_group = None
            return
        if window.sheets_in_group(group):
            return
        self.files_group = None
        if window.num_groups() == len(self.layout_before_files["cells"]) + 1:
            window.set_layout(self.layout_before_files)
            self.focus_composer()

    def ensure_chats_view(self):
        """The chat list; closing it leaves the chat open, and ☰ Chats brings it back."""
        view = self.views.get("chats")
        if view is not None and view.is_valid():
            return view
        group = getattr(self, "chats_group", 0)
        if group >= self.window.num_groups():
            group = 0
        view = self.new_view(group, "chats", "☰ Chats", CHATS_SYNTAX, {
            "word_wrap": False,
            "highlight_line": True,
            "caret_style": "solid",
        })
        self.views["chats"] = view
        return view

    def show_chats(self):
        if not self.alive():
            return
        view = self.ensure_chats_view()
        self.window.focus_view(view)
        self.refresh_chats()

    def focus_composer(self):
        if self.alive():
            self.window.focus_view(self.views["composer"])

    def new_view(self, group, role, name, syntax, extra):
        window = self.window
        window.focus_group(group)
        view = window.new_file()
        view.set_scratch(True)
        view.set_name(name)
        view.assign_syntax(syntax)
        view_settings = view.settings()
        for key, value in {
            "oxi_role": role,
            "line_numbers": False,
            "gutter": False,
            "margin": 6,
            "draw_indent_guides": False,
            "rulers": [],
            "scroll_past_end": False,
            "fold_buttons": False,
            "draw_white_space": "none",
            "spell_check": False,
            "auto_complete": False,
            "is_widget": False,
            "translate_tabs_to_spaces": True,
            "detect_indentation": False,
            "word_wrap": True,
        }.items():
            view_settings.set(key, value)
        for key, value in extra.items():
            view_settings.set(key, value)
        if role != "composer":
            view.set_read_only(True)
        return view

    def teardown(self):
        if self.closing:
            return
        self.closing = True
        PANELS.pop(self.window.id(), None)
        for view in self.views.values():
            if view.is_valid():
                view.close()
        restore_window(self.window)

    # ---- chat list ----------------------------------------------------------------------

    def refresh_chats(self):
        def done(result, error):
            if error or not self.alive():
                return
            self.chats = result or []
            self.render_chats()

        request("chats.list", {"root": self.root}, done)

    def render_chats(self):
        view = self.views.get("chats")
        if view is None or not view.is_valid():
            return
        text, rows = render.render_chat_list(self.chats, self.chat_id, time.time())
        self.chat_rows = rows
        replace(view, 0, view.size(), text)
        row = rows.index(self.chat_id) if self.chat_id in rows else 0
        line = view.line(view.text_point(row, 0))
        view.add_regions("oxi_current", [line], "region.bluish", "", sublime.DRAW_NO_OUTLINE)
        # Mid arrow walk, keep the caret on the chat about to open rather than the open one.
        pending = getattr(self, "chat_pending", False)
        if pending is not False and pending in rows:
            row = rows.index(pending)
        self.put_chat_caret(row)

    def put_chat_caret(self, row):
        """Caret at the start of a row, without the horizontal scroll a long title causes."""
        view = self.views["chats"]
        point = view.text_point(row, 0)
        view.sel().clear()
        view.sel().add(sublime.Region(point))
        view.show(point, False)
        x, y = view.viewport_position()
        if x:
            view.set_viewport_position((0, y), False)

    def chat_caret_row(self):
        view = self.views["chats"]
        selection = view.sel()
        if not selection:
            return None
        row, _ = view.rowcol(selection[0].begin())
        return row if row < len(self.chat_rows) else None

    def open_selected_chat(self, focus=True):
        row = self.chat_caret_row()
        if row is None:
            return
        chat = self.chat_rows[row]
        if chat is None:
            if self.chat_id is not None:
                self.new_chat()
        elif chat != self.chat_id:
            self.open_chat(chat)
        if focus:
            self.focus_composer()

    def click_chat(self, row):
        if row is None or row >= len(self.chat_rows):
            row = self.chat_caret_row()
            if row is not None:
                self.put_chat_caret(row)
            return
        self.put_chat_caret(row)
        self.open_selected_chat()

    def move_chat(self, delta):
        """Arrow keys walk the list; the chat opens once the caret rests for a moment."""
        row = self.chat_caret_row()
        row = 0 if row is None else max(0, min(len(self.chat_rows) - 1, row + delta))
        self.put_chat_caret(row)
        self.chat_pending = self.chat_rows[row]
        self.chat_move_serial = getattr(self, "chat_move_serial", 0) + 1
        serial = self.chat_move_serial

        def open_later():
            if serial == self.chat_move_serial and self.alive():
                self.chat_pending = False
                self.open_selected_chat(focus=False)

        sublime.set_timeout(open_later, 150)

    def delete_chat(self, chat_id):
        """Asks first; the chat is closed in every window that has it open (`chat_removed`)."""
        if chat_id is None:
            return
        title = self.title if chat_id == self.chat_id else next(
            (chat.get("title") for chat in self.chats if chat.get("id") == chat_id), None
        )
        if not sublime.ok_cancel_dialog("Delete the chat “{}”?".format(title or "Chat"), "Delete"):
            return
        request("chat.delete", {"root": self.root, "chat": chat_id})
        if chat_id == self.chat_id:
            self.new_chat()

    # ---- chats --------------------------------------------------------------------------

    def new_chat(self):
        self.window.settings().erase("oxi_last_chat")
        self.chat_id = None
        self.title = "New chat"
        self.messages = []
        self.running = False
        self.queued = 0
        self.approval = None
        self.status = None
        self.context = None
        self.loading = False
        self.error = None
        self.views["transcript"].set_name("oxi · New chat")
        self.render_all()
        self.update_approval()
        if self.chats:
            self.render_chats()

        def done(result, error):
            if not error and self.chat_id is None:
                self.config = result
                self.plan = False
                self.refresh_toolbar()

        request("chat.set_config", {"root": self.root}, done)
        self.refresh_toolbar()
        self.update_placeholder()

    def open_chat(self, chat_id, scroll=True):
        self.chat_id = chat_id
        self.loading = True
        self.window.settings().set("oxi_last_chat", chat_id)

        def done(result, error):
            if self.chat_id != chat_id or not self.alive():
                return
            self.loading = False
            if error:
                self.new_chat()
                self.show_error("Could not open the chat: " + error)
                return
            self.title = result.get("title") or "Chat"
            self.messages = result.get("messages") or []
            self.config = result.get("config")
            self.running = bool(result.get("running"))
            self.queued = result.get("queued") or 0
            self.plan = bool(result.get("plan"))
            self.approval = result.get("approval")
            self.status = result.get("status")
            self.context = result.get("context")
            self.error = None
            self.views["transcript"].set_name("oxi · " + render.one_line(self.title, 40))
            self.render_all(scroll=scroll)
            self.update_approval()
            self.refresh_toolbar()
            self.render_chats()

        request("chat.open", {"root": self.root, "chat": chat_id}, done)

    def set_title(self, title):
        self.title = title or "Chat"
        if self.alive():
            self.views["transcript"].set_name("oxi · " + render.one_line(self.title, 40))
        self.update_header()

    def send(self):
        composer = self.views["composer"]
        text = composer.substr(sublime.Region(0, composer.size())).strip()
        if not text and not self.images:
            return
        plan = None
        if text == "/new":
            replace(composer, 0, composer.size(), "")
            self.new_chat()
            return
        if text == "/plan":
            replace(composer, 0, composer.size(), "")
            self.toggle_plan()
            return
        if text.startswith("/plan "):
            plan = True
            text = text[len("/plan "):].strip()
        replace(composer, 0, composer.size(), "")
        self.error = None
        self.update_approval()
        params = {"root": self.root, "text": text}
        images, self.images = self.images, []
        if images:
            params["attachments"] = images
            self.refresh_toolbar()
        if self.chat_id is not None:
            params["chat"] = self.chat_id
        elif self.config:
            params.update({
                "provider": self.config.get("provider"),
                "model": self.config.get("model"),
                "effort": self.config.get("effort"),
            })
        if plan is not None:
            params["plan"] = plan
        elif self.plan:
            params["plan"] = True
        sent_to = self.chat_id

        def done(result, error):
            if error:
                if composer.is_valid() and composer.size() == 0:
                    replace(composer, 0, 0, text)
                if images and not self.images:
                    self.images = images
                    self.refresh_toolbar()
                self.show_error("The message was not sent: " + error)
                return
            if result.get("queued"):
                self.queued = result["queued"]
                self.refresh_toolbar()
            if sent_to is None and self.chat_id is None:
                self.open_chat(result["chat"])

        request("prompt.send", params, done)

    def show_error(self, text):
        self.error = text
        self.update_approval()
        self.update_placeholder()
        if self.alive():
            scroll_to_end(self.views["transcript"])
        sublime.status_message("oxi: " + render.one_line(text, 160))

    def update_placeholder(self):
        """Keep the toolbar under the last line of the message, and the hint in it while empty."""
        if not self.alive():
            return
        # A phantom stays where it was put while text is typed after it, so after a wrapped line
        # or a Shift+Enter the toolbar would sit between lines: put it back under the end.
        if self.views["composer"].size() != getattr(self, "toolbar_anchor", None):
            self.refresh_toolbar()

    def update_header(self):
        if not self.alive():
            return
        content = (
            '<body id="oxi-header">{style}<a class="pill" href="chats">☰ Chats</a> '
            '<a class="pill" href="new">＋ New chat</a>  <b>{title}</b></body>'
        ).format(style=TOOLBAR_STYLE, title=html.escape(render.one_line(self.title, 60)))
        self.phantoms["header"].update(
            [sublime.Phantom(sublime.Region(0), content, sublime.LAYOUT_BLOCK, self.on_link)]
        )

    def stop(self):
        if self.chat_id is not None:
            request("cancel", {"chat": self.chat_id})

    def toggle_plan(self):
        self.plan = not self.plan
        self.refresh_toolbar()
        sublime.status_message("oxi: plan mode " + ("on" if self.plan else "off"))

    def set_config(self, **fields):
        params = {"root": self.root}
        if self.chat_id is not None:
            params["chat"] = self.chat_id
        params.update(fields)
        chat = self.chat_id

        def done(result, error):
            if error:
                sublime.status_message("oxi: " + error)
            elif self.chat_id == chat:
                self.config = result
                self.refresh_toolbar()

        request("chat.set_config", params, done)

    # ---- streaming ----------------------------------------------------------------------

    def on_message(self, index, message):
        if index < len(self.messages):
            self.messages[index] = message
        elif index == len(self.messages):
            self.messages.append(message)
        else:
            # Missed something: reload the whole chat.
            self.open_chat(self.chat_id)
            return
        self.render_from(index)

    def on_turn(self, params):
        self.running = bool(params.get("running"))
        self.queued = params.get("queued") or 0
        self.plan = bool(params.get("plan"))
        outcome = params.get("outcome")
        if outcome:
            self.status = None
            self.refresh_quota()
            self.render_all()
            if outcome == "failed" and params.get("error"):
                sublime.status_message("oxi: " + render.one_line(params["error"], 160))
        self.refresh_toolbar()

    # ---- transcript rendering -----------------------------------------------------------

    def options(self):
        s = settings()
        return {
            "tool_output_lines": int(s.get("tool_output_lines", 4)),
            "show_thinking": bool(s.get("show_thinking", True)),
        }

    def render_all(self, scroll=False):
        view = self.views["transcript"]
        self.update_header()
        self.starts = []
        self.links = []
        self.history_upto = 0
        if not self.messages:
            replace(view, 0, view.size(), EMPTY_STATE)
            self.phantoms["history"].update([])
            self.phantoms["tail"].update([])
            return
        self.render_from(0, scroll=scroll)
        # A reply still streaming stays out of the history, so its updates re-render only it.
        upto = len(self.messages)
        if self.messages[-1].get("streaming"):
            upto -= 1
        self.history_upto = upto
        self.phantoms["history"].update(self.tool_phantoms(0, upto))
        self.phantoms["tail"].update(self.tool_phantoms(upto))

    def render_from(self, index, scroll=False):
        view = self.views["transcript"]
        if index < self.history_upto or index > len(self.starts):
            return self.render_all(scroll=scroll)
        if index < len(self.starts):
            begin = self.starts[index]
        else:
            begin = 0 if index == 0 else view.size()
        del self.starts[index:]
        self.links = [link for link in self.links if link[2] < index]
        follow = scroll or at_bottom(view)
        options = self.options()
        parts = []
        position = begin
        for i in range(index, len(self.messages)):
            rendered = render.render_message(self.messages[i], options)
            self.starts.append(position)
            for start, end, block, kind in rendered.links:
                self.links.append((position + start, position + end, i, block, kind))
            parts.append(rendered.text)
            position += rendered.length
        replace(view, begin, view.size(), "".join(parts))
        if index >= self.history_upto:
            self.phantoms["tail"].update(self.tool_phantoms(self.history_upto))
        self.update_approval()
        if follow:
            scroll_to_end(view)

    def tool_phantoms(self, first_message, end_message=None):
        phantoms = []
        for start, end, message, block_index, kind in self.links:
            if message < first_message or kind not in ("tool", "user"):
                continue
            if end_message is not None and message >= end_message:
                continue
            if kind == "user":
                images = self.messages[message].get("images") or []
                actions = [
                    '<a href="image:{}:{}">▣ image {}</a>'.format(message, image.get("index"), n + 1)
                    for n, image in enumerate(images)
                ]
                if actions:
                    content = '<body id="oxi-tool">{}&nbsp;&nbsp;{}</body>'.format(LINK_STYLE, " · ".join(actions))
                    phantoms.append(sublime.Phantom(sublime.Region(end), content, sublime.LAYOUT_INLINE, self.on_link))
                continue
            block = self.block(message, block_index)
            if block is None:
                continue
            actions = []
            if block.get("diff"):
                actions.append('<a href="details:{}:{}">diff</a>'.format(message, block_index))
            if block.get("locations"):
                actions.append('<a href="open:{}:{}">open</a>'.format(message, block_index))
            if not actions:
                continue
            content = '<body id="oxi-tool">{}&nbsp;&nbsp;{}</body>'.format(LINK_STYLE, " · ".join(actions))
            phantoms.append(sublime.Phantom(sublime.Region(end), content, sublime.LAYOUT_INLINE, self.on_link))
        return phantoms

    def block(self, message, block_index):
        try:
            return self.messages[message]["blocks"][block_index]
        except (IndexError, KeyError, TypeError):
            return None

    def update_approval(self):
        view = self.views["transcript"]
        approval = self.approval
        if not approval:
            phantoms = []
            if self.error:
                content = '<body id="oxi-error">{}<div class="card">{}</div></body>'.format(
                    ERROR_STYLE, html.escape(self.error).replace("\n", "<br>")
                )
                phantoms.append(sublime.Phantom(sublime.Region(view.size()), content, sublime.LAYOUT_BLOCK))
            self.phantoms["approval"].update(phantoms)
            return
        actions = [
            '<a class="primary" href="approve">Allow</a>',
            '<a href="approve_rest">Allow for this turn</a>',
        ]
        prefix = approval.get("allow_prefix")
        if prefix:
            actions.append('<a href="allow_prefix">Always allow <code>{}</code></a>'.format(html.escape(prefix)))
        tool = approval.get("tool") or ""
        if tool and tool != "bash":
            actions.append('<a href="allow_tool">Always allow here</a>')
        actions.append('<a class="deny" href="deny">Deny</a>')
        summary = approval.get("summary") or ""
        content = (
            '<body id="oxi-approval">{style}<div class="card">'
            "<b>oxi wants to run {tool}</b>{summary}<br><br>{actions}</div></body>"
        ).format(
            style=APPROVAL_STYLE,
            tool=html.escape(tool or "a tool"),
            summary="<br><code>{}</code>".format(html.escape(summary)) if summary else "",
            actions="&nbsp;&nbsp;".join(actions),
        )
        phantom = sublime.Phantom(sublime.Region(view.size()), content, sublime.LAYOUT_BLOCK, self.on_link)
        self.phantoms["approval"].update([phantom])

    def refresh_toolbar(self):
        if not self.alive():
            return
        composer = self.views["composer"]
        config = self.config or {}
        provider = config.get("label") or provider_label(config.get("provider"))
        model = config.get("model") or "default model"
        effort = config.get("effort") or "default effort"
        items = [
            '<a class="pill" href="provider">◆ {}</a>'.format(html.escape(provider)),
            '<a class="pill" href="model">{}</a>'.format(html.escape(render.one_line(model, 40))),
            '<a class="pill" href="effort">{}</a>'.format(html.escape(effort)),
            '<a class="pill{}" href="plan">plan {}</a>'.format(" on" if self.plan else "", "on" if self.plan else "off"),
        ]
        if self.running:
            items.append('<a class="pill stop" href="stop">■ stop</a>')
        items.append('<a class="pill" href="new">＋ new</a>')
        notes = []
        if self.approval:
            notes.append("waiting for approval")
        elif self.running:
            notes.append("working…")
        if self.queued:
            notes.append("{} queued".format(self.queued))
        if self.status:
            notes.append(render.one_line(self.status, 80))
        if self.context and self.context.get("size"):
            notes.append("ctx {}/{}".format(
                render.format_tokens(self.context.get("used")),
                render.format_tokens(self.context.get("size")),
            ))
        if composer.size() == 0 and not notes:
            notes.append("Enter sends · Shift+Enter new line · @ attaches a file · paste an image")
        attached = "".join(
            '<a class="pill" href="image-open:{0}">▣ image {1}</a><a class="muted" href="image-remove:{0}"> ✕</a> '.format(i, i + 1)
            for i in range(len(self.images))
        )
        if config.get("provider") != self.quota_provider:
            self.quota = None
            self.refresh_quota()
        quota = self.quota_html()
        content = '<body id="oxi-toolbar">{}{}{} <span class="muted">{}</span>{}</body>'.format(
            TOOLBAR_STYLE, attached + "<br>" if attached else "", " ".join(items), html.escape(" · ".join(notes)),
            '<br><span class="muted">usage</span> ' + quota if quota else "",
        )
        # Under the last line, so it never splits a multi-line message.
        end = composer.size()
        self.toolbar_anchor = end
        phantom = sublime.Phantom(sublime.Region(end), content, sublime.LAYOUT_BLOCK, self.on_link)
        self.phantoms["toolbar"].update([phantom])
        composer.settings().set("oxi_running", self.running)
        status = "oxi: {} · {}".format(provider, model)
        if notes:
            status += " · " + " · ".join(notes)
        for view in self.views.values():
            if view.is_valid():
                view.set_status("oxi", status)

    # ---- links and details --------------------------------------------------------------

    def on_link(self, href):
        if href in ("approve", "approve_rest", "deny", "allow_prefix", "allow_tool"):
            self.respond(href)
        elif href == "provider":
            self.pick_provider()
        elif href == "model":
            self.pick_model()
        elif href == "effort":
            self.pick_effort()
        elif href == "plan":
            self.toggle_plan()
        elif href == "stop":
            self.stop()
        elif href == "new":
            self.new_chat()
            self.focus_composer()
        elif href == "chats":
            self.show_chats()
        elif href.startswith("image-open:"):
            index = int(href.split(":")[1])
            if index < len(self.images):
                image = self.images[index]
                self.show_image(image["data"], image["mime"], "attached-{}".format(index + 1))
        elif href.startswith("image-remove:"):
            index = int(href.split(":")[1])
            if index < len(self.images):
                del self.images[index]
                self.refresh_toolbar()
        elif ":" in href:
            action, message, block = href.split(":", 2)
            if action == "image":
                self.open_message_image(int(message), int(block))
            elif action == "details":
                self.open_details(int(message), int(block))
            elif action == "open":
                self.open_location(int(message), int(block))

    def respond(self, decision):
        if not self.approval or self.chat_id is None:
            return
        params = {"chat": self.chat_id, "decision": decision}
        if decision == "allow_prefix":
            params["prefix"] = self.approval.get("allow_prefix")
        self.approval = None
        self.update_approval()
        self.refresh_toolbar()
        request("approval.respond", params)
        self.focus_composer()

    def link_at(self, point):
        for start, end, message, block, kind in self.links:
            if start <= point <= end:
                return message, block, kind
        return None

    def open_details_at_caret(self):
        view = self.views["transcript"]
        if not view.sel():
            return
        line = view.line(view.sel()[0].begin())
        found = self.link_at(line.begin())
        if found is None:
            return
        if found[2] == "user":
            images = self.messages[found[0]].get("images") or []
            if images:
                self.open_message_image(found[0], images[0].get("index"))
        else:
            self.open_details(found[0], found[1])

    def open_details(self, message, block_index):
        block = self.block(message, block_index)
        if block is None:
            return
        if block.get("type") == "thinking":
            text = "# Thinking\n\n" + (block.get("text") or "") + "\n"
            name = "oxi · thinking"
        else:
            text = render.tool_details(block)
            name = "oxi · {}".format(render.one_line(block.get("title") or block.get("name") or "tool", 40))
        window = self.window
        window.focus_group(self.editor_group())
        view = window.new_file()
        view.set_scratch(True)
        view.set_name(name)
        view.assign_syntax("Packages/Markdown/Markdown.sublime-syntax")
        replace(view, 0, 0, text)
        view.set_read_only(True)

    def open_location(self, message, block_index):
        block = self.block(message, block_index) or {}
        locations = block.get("locations") or []
        if not locations:
            return
        location = locations[0]
        path = location.get("path") or ""
        if not os.path.isabs(path):
            path = os.path.join(self.root, path)
        line = location.get("line")
        target = "{}:{}".format(path, line) if line else path
        self.window.focus_group(self.editor_group())
        self.window.open_file(target, sublime.ENCODED_POSITION)

    # ---- images -------------------------------------------------------------------------

    def attach_image(self, mime, data):
        if len(self.images) >= MAX_IMAGES:
            sublime.status_message("oxi: at most {} images per message".format(MAX_IMAGES))
            return
        self.images.append({"mime": mime, "data": data})
        self.refresh_toolbar()
        self.focus_composer()

    def paste_image(self):
        """Attach the image in the clipboard; oxi reads it, as Sublime only sees text there."""

        def done(result, error):
            if error:
                sublime.status_message("oxi: " + error)
            elif not result:
                sublime.status_message("oxi: no image in the clipboard")
            elif self.alive():
                self.attach_image(result["mime"], result["data"])

        request("clipboard.image", {}, done)

    def pick_images(self):
        def chosen(paths):
            if not paths:
                return
            for path in paths if isinstance(paths, list) else [paths]:
                self.attach_image_file(path)

        types = [("Images", sorted(IMAGE_TYPES))]
        sublime.open_dialog(chosen, file_types=types, directory=self.root, multi_select=True)

    def attach_image_file(self, path):
        mime = IMAGE_TYPES.get(os.path.splitext(path)[1].lstrip(".").lower())
        if mime is None:
            sublime.status_message("oxi: not an image: " + os.path.basename(path))
            return
        try:
            with open(path, "rb") as f:
                data = f.read(MAX_IMAGE_BYTES + 1)
        except OSError as e:
            sublime.status_message("oxi: could not read {}: {}".format(os.path.basename(path), e))
            return
        if len(data) > MAX_IMAGE_BYTES:
            sublime.status_message("oxi: image too large (max {} MB)".format(MAX_IMAGE_BYTES // (1024 * 1024)))
            return
        self.attach_image(mime, base64.b64encode(data).decode("ascii"))

    def open_message_image(self, message, index):
        chat = self.chat_id

        def done(result, error):
            if error:
                sublime.status_message("oxi: " + error)
            elif result and self.alive():
                key = hashlib.sha1(chat.encode("utf-8")).hexdigest()[:10]
                self.show_image(result["data"], result["mime"], "{}-{}-{}".format(key, message, index))

        if chat is not None:
            request("chat.image", {"root": self.root, "chat": chat, "message": message, "index": index}, done)

    def show_image(self, data, mime, name):
        """Open an image in the editor column; Sublime shows image files in an image tab."""
        directory = os.path.join(tempfile.gettempdir(), "oxi-images")
        extension = {"image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp"}.get(mime, "png")
        path = os.path.join(directory, "{}.{}".format(name, extension))
        try:
            os.makedirs(directory, exist_ok=True)
            with open(path, "wb") as f:
                f.write(base64.b64decode(data))
        except (OSError, ValueError) as e:
            sublime.status_message("oxi: could not show the image: {}".format(e))
            return
        self.window.focus_group(self.editor_group())
        self.window.open_file(path)

    # ---- pickers ------------------------------------------------------------------------

    def pick_provider(self):
        def show(result, error):
            if error:
                sublime.status_message("oxi: " + error)
                return
            PROVIDERS[:] = result.get("providers") or []
            current = (self.config or {}).get("provider")
            items = []
            selected = 0
            for i, provider in enumerate(PROVIDERS):
                if provider.get("id") == current:
                    selected = i
                items.append(sublime.QuickPanelItem(
                    provider.get("label") or provider.get("id"),
                    annotation=provider.get("model") or "",
                    details=provider.get("description") or "",
                ))

            def chosen(index):
                if index >= 0:
                    self.set_config(provider=PROVIDERS[index]["id"])

            self.window.show_quick_panel(items, chosen, selected_index=selected, placeholder="Agent or provider")

        request("initialize", {}, show)

    def pick_model(self):
        config = self.config or {}
        provider = config.get("provider")
        current = config.get("model") or ""

        def ask():
            self.window.show_input_panel(
                "Model:", current, lambda model: self.set_config(model=model), None, None
            )

        def show(result, error):
            self.status = None
            self.refresh_toolbar()
            models = (result or {}).get("models") or []
            if error or not models:
                ask()
                return
            items = models + ["Other…"]
            selected = models.index(current) if current in models else 0

            def chosen(index):
                if index < 0:
                    return
                if index == len(models):
                    ask()
                else:
                    self.set_config(model=models[index])

            self.window.show_quick_panel(items, chosen, selected_index=selected, placeholder="Model")

        params = {"root": self.root, "provider": provider}
        if self.chat_id is not None:
            params["chat"] = self.chat_id
        self.status = "loading models…"
        self.refresh_toolbar()
        request("models.list", params, show)

    def pick_effort(self):
        current = (self.config or {}).get("effort") or ""
        labels = [effort or "default" for effort in EFFORTS]

        def chosen(index):
            if index >= 0:
                self.set_config(effort=EFFORTS[index])

        selected = EFFORTS.index(current) if current in EFFORTS else 0
        self.window.show_quick_panel(labels, chosen, selected_index=selected, placeholder="Reasoning effort")


EMPTY_STATE = (
    "\nNew chat. Type your message in the box below and press Enter.\n\n"
    "  @path        attach a file or folder from the project\n"
    "  Cmd/Ctrl+V   paste a screenshot or image as an attachment\n"
    "  /plan        toggle plan mode (or /plan <task>)\n"
    "  /new         start another chat\n"
    "  Shift+Enter  new line\n"
)

LINK_STYLE = "<style>a { text-decoration: none; }</style>"

ERROR_STYLE = """<style>
.card { background-color: color(var(--redish) alpha(0.14)); border-left: 0.25rem solid var(--redish); padding: 0.6rem 0.8rem; margin: 0.4rem 0; }
</style>"""

TOOLBAR_STYLE = """<style>
body { margin: 0.1rem 0 0.35rem 0; padding: 0; }
a { text-decoration: none; color: var(--foreground); }
.pill { background-color: color(var(--foreground) alpha(0.08)); padding: 0.05rem 0.45rem; border-radius: 0.3rem; }
.on { background-color: color(var(--bluish) alpha(0.3)); }
.stop { color: var(--redish); }
.muted { color: color(var(--foreground) alpha(0.55)); }
.high { color: var(--yellowish); }
.full { color: var(--redish); }
</style>"""

# `quota.get` is throttled in oxi, so polling only rereads usage now and then.
QUOTA_POLL_MS = 120000

APPROVAL_STYLE = """<style>
.card { background-color: color(var(--yellowish) alpha(0.12)); border-left: 0.25rem solid var(--yellowish); padding: 0.6rem 0.8rem; margin: 0.4rem 0; }
code { background-color: color(var(--foreground) alpha(0.08)); padding: 0 0.2rem; }
a { text-decoration: none; padding: 0.1rem 0.5rem; background-color: color(var(--foreground) alpha(0.1)); border-radius: 0.25rem; color: var(--foreground); }
a.primary { background-color: color(var(--bluish) alpha(0.4)); }
a.deny { color: var(--redish); }
</style>"""


SCHEME_DIR = "oxi-transcript-colors"


def active_scheme_name():
    """File name of the color scheme in use, resolving "auto" to the light or dark one."""
    try:
        name = sublime.ui_info()["color_scheme"]["resolved_value"]
    except (AttributeError, KeyError, TypeError):
        name = sublime.load_settings("Preferences.sublime-settings").get("color_scheme")
    if not name:
        return None
    return os.path.splitext(os.path.basename(name))[0] + ".sublime-color-scheme"


def update_transcript_colors(view=None):
    """Merge Markdown-preview rules for the transcript into the active color scheme.

    Sublime layers every `.sublime-color-scheme` file with the same name, so a file named after
    the user's scheme in Packages/User adds rules without touching the scheme. The rules only
    match oxi transcripts.
    """
    if not settings().get("markdown_preview", True):
        return
    name = active_scheme_name()
    if view is None:
        window = sublime.active_window()
        view = window.active_view() if window else None
    if not name or view is None:
        return
    style = view.style()
    accent = view.style_for_scope("entity.name.function").get("foreground")
    code = view.style_for_scope("string").get("foreground")
    rules = scheme.rules(style.get("foreground"), style.get("background"), accent, code)
    if not rules:
        return
    text = json.dumps({"rules": rules}, indent=2) + "\n"
    folder = os.path.join(sublime.packages_path(), "User", SCHEME_DIR)
    path = os.path.join(folder, name)
    try:
        with open(path) as f:
            if f.read() == text:
                return
    except OSError:
        pass
    try:
        os.makedirs(folder, exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
    except OSError as error:
        log("could not write transcript colors: {}".format(error))


def remove_transcript_colors():
    shutil.rmtree(os.path.join(sublime.packages_path(), "User", SCHEME_DIR), ignore_errors=True)


def restore_window(window):
    """Put back the layout and minimap the window had before the oxi column opened."""
    window_settings = window.settings()
    saved = window_settings.get("oxi_saved_layout")
    if saved:
        window.set_layout(saved)
        window_settings.erase("oxi_saved_layout")
    minimap = window_settings.get("oxi_saved_minimap")
    if minimap is not None:
        window.set_minimap_visible(minimap)
        window_settings.erase("oxi_saved_minimap")


def replace(view, begin, end, text):
    view.run_command("oxi_replace", {"begin": begin, "end": end, "text": text})


def project_files(root):
    cached = FILE_CACHE.get(root)
    if cached and time.time() - cached[0] < 60:
        return cached[1]
    files = []
    for directory, dirs, names in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in IGNORED_DIRS and not d.startswith("."))
        relative = os.path.relpath(directory, root)
        for name in sorted(names):
            files.append(name if relative == "." else os.path.join(relative, name).replace(os.sep, "/"))
        if len(files) >= MAX_MENTION_FILES:
            break
    FILE_CACHE[root] = (time.time(), files)
    return files


# ---- commands -----------------------------------------------------------------------------


class OxiReplaceCommand(sublime_plugin.TextCommand):
    """Edit an oxi view, read-only or not."""

    def run(self, edit, begin, end, text):
        view = self.view
        read_only = view.is_read_only()
        carets = [r.b for r in view.sel()] if all(r.empty() for r in view.sel()) else None
        view.set_read_only(False)
        view.replace(edit, sublime.Region(begin, min(end, view.size())), text)
        view.set_read_only(read_only)
        # Replacing text under a caret selects the new text; keep plain carets plain.
        if carets is not None and read_only:
            view.sel().clear()
            for point in carets:
                view.sel().add(sublime.Region(min(point, view.size())))


class OxiOpenCommand(sublime_plugin.WindowCommand):
    """Show the oxi column (and focus the composer)."""

    def run(self):
        panel = panel_for(self.window, create=True)
        if panel is not None:
            panel.focus_composer()


class OxiToggleCommand(sublime_plugin.WindowCommand):
    """Open the column, focus its composer, or (when already typing there) close it."""

    def run(self):
        panel = panel_for(self.window)
        if panel is None:
            panel_for(self.window, create=True)
        elif self.window.active_view() != panel.views["composer"]:
            panel.focus_composer()
        else:
            panel.teardown()


class OxiCloseCommand(sublime_plugin.WindowCommand):
    def run(self):
        panel = panel_for(self.window)
        if panel is not None:
            panel.teardown()

    def is_enabled(self):
        return panel_for(self.window) is not None


class PanelCommand(sublime_plugin.WindowCommand):
    """Base for commands acting on the window's oxi column, opening it if needed."""

    needs_chat = False

    def run(self, **args):
        panel = panel_for(self.window, create=True)
        if panel is not None:
            self.act(panel, **args)

    def is_enabled(self, **args):
        if not self.needs_chat:
            return True
        panel = panel_for(self.window)
        return panel is not None and panel.chat_id is not None

    def act(self, panel, **args):
        raise NotImplementedError


class OxiNewChatCommand(PanelCommand):
    def act(self, panel):
        panel.new_chat()
        panel.focus_composer()


class OxiComposerSendCommand(sublime_plugin.TextCommand):
    """Enter in the composer (a text command, so key bindings and on_text_command can use it)."""

    def run(self, edit):
        panel = view_panel(self.view)
        if panel is not None:
            panel.send()


class OxiNewlineCommand(sublime_plugin.TextCommand):
    """Shift+Enter in the composer: a new line without sending."""

    def run(self, edit):
        # `insert` would be caught by the Enter-sends fallback, and `replace` leaves the caret
        # before the new line, so insert and place the carets by hand.
        view = self.view
        carets = []
        shift = 0
        for region in sorted(view.sel(), key=lambda r: r.begin()):
            begin = region.begin() + shift
            view.erase(edit, sublime.Region(begin, region.end() + shift))
            view.insert(edit, begin, "\n")
            shift += 1 - region.size()
            carets.append(begin + 1)
        view.sel().clear()
        for point in carets:
            view.sel().add(sublime.Region(point))
        view.show(carets[-1] if carets else 0)


class OxiSendCommand(PanelCommand):
    def act(self, panel):
        panel.send()


class OxiStopCommand(PanelCommand):
    needs_chat = True

    def act(self, panel):
        panel.stop()


class OxiTogglePlanCommand(PanelCommand):
    def act(self, panel):
        panel.toggle_plan()


class OxiChooseProviderCommand(PanelCommand):
    def act(self, panel):
        panel.pick_provider()


class OxiChooseModelCommand(PanelCommand):
    def act(self, panel):
        panel.pick_model()


class OxiChooseEffortCommand(PanelCommand):
    def act(self, panel):
        panel.pick_effort()


class OxiApprovalCommand(PanelCommand):
    needs_chat = True

    def act(self, panel, decision="approve"):
        panel.respond(decision)


class OxiSwitchChatCommand(PanelCommand):
    def act(self, panel):
        def show(result, error):
            if error:
                sublime.status_message("oxi: " + error)
                return
            chats = result or []
            now = time.time()
            items = [
                sublime.QuickPanelItem(
                    chat.get("title") or "Chat",
                    annotation=render.format_age(chat.get("modified"), now),
                    details=provider_label(chat.get("provider")) + (" · running" if chat.get("running") else ""),
                )
                for chat in chats
            ]

            def chosen(index):
                if index >= 0:
                    panel.open_chat(chats[index]["id"])
                    panel.focus_composer()

            self.window.show_quick_panel(items, chosen, placeholder="Open an oxi chat")

        request("chats.list", {"root": panel.root}, show)


class OxiRenameChatCommand(PanelCommand):
    needs_chat = True

    def act(self, panel):
        chat = panel.chat_id

        def done(title):
            request("chat.rename", {"root": panel.root, "chat": chat, "title": title})
            if panel.chat_id == chat:
                panel.title = title
                panel.views["transcript"].set_name("oxi · " + render.one_line(title, 40))
                panel.update_header()

        self.window.show_input_panel("Chat title:", panel.title, done, None, None)


class OxiDeleteChatCommand(PanelCommand):
    needs_chat = True

    def act(self, panel):
        panel.delete_chat(panel.chat_id)


class OxiDeleteListedChatCommand(sublime_plugin.TextCommand):
    """Delete / Backspace in the chat list, or its context menu: the chat under the caret or
    the one right-clicked."""

    def run(self, edit, event=None):
        panel = view_panel(self.view)
        row = self.row(event)
        if panel is not None and row is not None and row < len(panel.chat_rows):
            panel.delete_chat(panel.chat_rows[row])

    def row(self, event):
        if event:
            return self.view.rowcol(self.view.window_to_text((event["x"], event["y"])))[0]
        selection = self.view.sel()
        return self.view.rowcol(selection[0].begin())[0] if selection else None

    def is_visible(self, event=None):
        return is_role(self.view, "chats")

    def is_enabled(self, event=None):
        panel = view_panel(self.view)
        row = self.row(event)
        return panel is not None and row is not None and 0 < row < len(panel.chat_rows)

    def want_event(self):
        return True


class OxiAttachImageCommand(PanelCommand):
    def act(self, panel):
        panel.pick_images()


class OxiPasteImageCommand(sublime_plugin.TextCommand):
    """Paste into the composer with no text in the clipboard, or from the command palette."""

    def run(self, edit):
        panel = panel_for(self.view.window(), create=True)
        if panel is not None:
            panel.paste_image()


class OxiOpenSelectedChatCommand(sublime_plugin.TextCommand):
    def run(self, edit):
        panel = view_panel(self.view)
        if panel is not None:
            panel.open_selected_chat()


class OxiChatMoveCommand(sublime_plugin.TextCommand):
    def run(self, edit, by):
        panel = view_panel(self.view)
        if panel is not None:
            panel.move_chat(by)


class OxiChatClickCommand(sublime_plugin.TextCommand):
    def run(self, edit, row=None):
        panel = view_panel(self.view)
        if panel is not None:
            panel.click_chat(row)


class OxiOpenDetailsCommand(sublime_plugin.TextCommand):
    def run(self, edit):
        panel = view_panel(self.view)
        if panel is not None:
            panel.open_details_at_caret()


class OxiInsertMentionCommand(sublime_plugin.TextCommand):
    """Completion target: replace `@partial` before the caret with `@path `."""

    def run(self, edit, path):
        view = self.view
        for region in view.sel():
            line = view.line(region.begin())
            before = view.substr(sublime.Region(line.begin(), region.begin()))
            at = before.rfind("@")
            if at < 0:
                view.insert(edit, region.begin(), "@" + path + " ")
                continue
            view.replace(edit, sublime.Region(line.begin() + at, region.begin()), "@" + path + " ")


class OxiAddToChatCommand(sublime_plugin.TextCommand):
    """Put the selection (or a mention of the file) into the composer."""

    def run(self, edit):
        view = self.view
        window = view.window()
        panel = panel_for(window, create=True)
        if panel is None:
            return
        path = view.file_name()
        relative = None
        if path:
            relative = os.path.relpath(path, panel.root)
            if relative.startswith(".."):
                relative = path
            relative = relative.replace(os.sep, "/")
        pieces = []
        for region in view.sel():
            if region.empty():
                continue
            first = view.rowcol(region.begin())[0] + 1
            last = view.rowcol(region.end())[0] + 1
            syntax = view.syntax()
            language = syntax.name.lower().split(" ")[0] if syntax else ""
            where = "{}:{}-{}".format(relative or "selection", first, last)
            pieces.append("{}\n```{}\n{}\n```\n".format(where, language, view.substr(region)))
        if not pieces:
            if not relative:
                return
            pieces.append("@{} ".format(relative))
        composer = panel.views["composer"]
        text = "".join(pieces)
        if composer.size() and not composer.substr(composer.size() - 1).isspace():
            text = "\n" + text
        replace(composer, composer.size(), composer.size(), text)
        window.focus_view(composer)
        composer.sel().clear()
        composer.sel().add(sublime.Region(composer.size()))


class OxiShowLogCommand(sublime_plugin.WindowCommand):
    def run(self):
        view = self.window.new_file()
        view.set_scratch(True)
        view.set_name("oxi · log")
        replace(view, 0, 0, "\n".join(LOG) + "\n")


class OxiRestartCommand(sublime_plugin.WindowCommand):
    def run(self):
        global CLIENT, READY
        if CLIENT is not None:
            client, CLIENT, READY = CLIENT, None, False
            # oxi runs as one daemon shared by every editor: stop it, not just this connection.
            # Turns running in other windows or editors are cancelled with it.
            client.request("daemon.stop")
            client.stop()
        for panel in list(PANELS.values()):
            panel.running = False
            panel.approval = None
            panel.status = None
            panel.refresh_toolbar()
            panel.update_approval()
            panel.refresh_chats()


# ---- events -------------------------------------------------------------------------------


class OxiEvents(sublime_plugin.EventListener):
    def on_query_context(self, view, key, operator, operand, match_all):
        if not key.startswith("oxi."):
            return None
        role = view.settings().get("oxi_role")
        if key == "oxi.running":
            value = bool(view.settings().get("oxi_running"))
        else:
            value = role == key[len("oxi."):]
        expected = True if operand is None else operand
        if operator == sublime.OP_NOT_EQUAL:
            return value != expected
        return value == expected

    def on_text_command(self, view, command, args):
        # A click in the chat list picks the row under the pointer and leaves the caret at its
        # start; a click below the last chat is ignored instead of jumping to the end of it.
        if command == "drag_select" and is_role(view, "chats"):
            event = (args or {}).get("event")
            if not event or (args or {}).get("additive") or (args or {}).get("extend"):
                return ("oxi_chat_click", {})
            point = view.window_to_text((event["x"], event["y"]))
            row, _ = view.rowcol(point)
            bottom = view.text_to_window(view.line(point).begin())[1] + view.line_height()
            return ("oxi_chat_click", {"row": row if event["y"] < bottom else None})
        # Sublime's clipboard holds text only: a paste with none of it may be a screenshot,
        # which oxi reads and attaches.
        if command == "paste" and is_role(view, "composer") and not sublime.get_clipboard():
            return ("oxi_paste_image", {})
        # Safety net: whatever binding another package gives Enter, a plain new line typed
        # into the composer sends the message (Shift+Enter uses `oxi_newline`).
        if (
            command == "insert"
            and (args or {}).get("characters") == "\n"
            and is_role(view, "composer")
            and not view.is_auto_complete_visible()
        ):
            return ("oxi_composer_send", {})
        return None

    def on_modified(self, view):
        if is_role(view, "composer"):
            panel = view_panel(view)
            if panel is not None:
                panel.update_placeholder()

    def on_pre_close(self, view):
        window = view.window()
        if window is not None and view.settings().get("oxi_role") is None:
            panel = PANELS.get(window.id())
            if panel is not None and getattr(panel, "files_group", None) is not None:
                sublime.set_timeout(panel.collapse_files, 0)
        # The Chats tab may come and go; closing the chat or the composer closes the column.
        if view.settings().get("oxi_role") in ("transcript", "composer"):
            panel = view_panel(view)
            if panel is not None and not panel.closing:
                sublime.set_timeout(panel.teardown, 0)

    def on_post_text_command(self, view, command, args):
        if command != "drag_select":
            return
        if is_role(view, "transcript") and (args or {}).get("by") == "words":
            panel = view_panel(view)
            if panel is not None:
                panel.open_details_at_caret()

    def on_query_completions(self, view, prefix, locations):
        if not is_role(view, "composer") or not locations:
            return None
        point = locations[0] - len(prefix)
        line = view.line(point)
        before = view.substr(sublime.Region(line.begin(), point))
        at = before.rfind("@")
        # Outside an @mention the composer offers nothing: no word or snippet completions.
        nothing = sublime.CompletionList([], sublime.INHIBIT_WORD_COMPLETIONS | sublime.INHIBIT_EXPLICIT_COMPLETIONS)
        if at < 0 or any(c.isspace() for c in before[at:]):
            return nothing
        panel = view_panel(view)
        if panel is None:
            return nothing
        typed = before[at + 1:] + prefix
        items = []
        for path in project_files(panel.root):
            if typed and typed.lower() not in path.lower():
                continue
            items.append(sublime.CompletionItem.command_completion(
                path, "oxi_insert_mention", {"path": path}, annotation="file",
                kind=(sublime.KIND_ID_NAVIGATION, "f", "File"),
            ))
            if len(items) >= 400:
                break
        return sublime.CompletionList(items, sublime.INHIBIT_WORD_COMPLETIONS | sublime.INHIBIT_EXPLICIT_COMPLETIONS)

    def on_window_command(self, window, command, args):
        # Closing the window with the column open: forget it without touching the layout,
        # which Sublime saves with the window.
        if command == "close_window":
            panel = PANELS.pop(window.id(), None)
            if panel is not None:
                panel.closing = True

    def on_post_window_command(self, window, command, args):
        # Images open as image sheets, which never reach `on_pre_close`: catch their tab
        # closing here so the files column goes away as it does for text files.
        if command.startswith("close"):
            panel = PANELS.get(window.id())
            if panel is not None and getattr(panel, "files_group", None) is not None:
                sublime.set_timeout(panel.collapse_files, 0)


def plugin_loaded():
    preferences = sublime.load_settings("Preferences.sublime-settings")
    preferences.clear_on_change("oxi")
    preferences.add_on_change("oxi", lambda: sublime.set_timeout(update_transcript_colors, 200))
    settings().clear_on_change("oxi")
    settings().add_on_change("oxi", on_settings_change)
    sublime.set_timeout(update_transcript_colors, 500)
    # Columns left over from the last session have no oxi behind them: remove them.
    for window in sublime.windows():
        stale = [v for v in window.views() if v.settings().get("oxi_role")]
        for view in stale:
            view.close()
        restore_window(window)


def on_settings_change():
    if settings().get("markdown_preview", True):
        update_transcript_colors()
    else:
        remove_transcript_colors()


def plugin_unloaded():
    sublime.load_settings("Preferences.sublime-settings").clear_on_change("oxi")
    settings().clear_on_change("oxi")
    global CLIENT
    for panel in list(PANELS.values()):
        panel.teardown()
    if CLIENT is not None:
        CLIENT.stop()
        CLIENT = None

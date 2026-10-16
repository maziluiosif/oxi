"""Turns chat messages from `oxi headless` into transcript text.

Pure Python (no `sublime` import) so it can be tested outside the editor. The line prefixes
used here are what `OxiTranscript.sublime-syntax` highlights; keep the two in sync.
"""

import json
import re
import unicodedata

USER_HEADER = "▌ You"
ASSISTANT_MARK = "◆ "
TOOL_MARK = "  ⏺ "
OUTPUT_MARK = "    │ "
THINKING_MARK = "  ✻ "
FOOTER_MARK = "  ─ "
TODO_MARKS = {"completed": "  ☑ ", "in_progress": "  ◐ ", "pending": "  ☐ "}

STATUS_GLYPHS = {
    "completed": "✓",
    "failed": "✗",
    "interrupted": "■",
    "pending": "…",
    "in_progress": "…",
}

# Tools whose output is worth a glance in the transcript; the rest only show a line.
OUTPUT_KINDS = {"execute"}
OUTPUT_NAMES = {"bash", "Bash", "terminal"}


def format_duration(secs):
    secs = int(secs or 0)
    if secs < 60:
        return "{}s".format(secs)
    minutes, secs = divmod(secs, 60)
    if minutes < 60:
        return "{}m {}s".format(minutes, secs)
    hours, minutes = divmod(minutes, 60)
    return "{}h {}m".format(hours, minutes)


def format_age(modified, now):
    delta = max(0, int(now - (modified or 0)))
    if delta < 60:
        return "now"
    if delta < 3600:
        return "{}m".format(delta // 60)
    if delta < 86400:
        return "{}h".format(delta // 3600)
    if delta < 86400 * 30:
        return "{}d".format(delta // 86400)
    return "{}mo".format(delta // (86400 * 30))


def format_tokens(n):
    n = int(n or 0)
    if n >= 1000000:
        return "{:.1f}M".format(n / 1000000.0).replace(".0M", "M")
    if n >= 1000:
        return "{}k".format(n // 1000)
    return str(n)


def format_duration(seconds):
    """Compact time left: 45m, 3h20m, 2d4h."""
    minutes = max(1, int(seconds or 0) // 60)
    if minutes < 60:
        return "{}m".format(minutes)
    hours, minutes = divmod(minutes, 60)
    if hours < 24:
        return "{}h{:02d}m".format(hours, minutes) if minutes else "{}h".format(hours)
    days, hours = divmod(hours, 24)
    return "{}d{}h".format(days, hours) if hours else "{}d".format(days)


def diff_stats(diff):
    added = removed = 0
    for line in (diff or "").splitlines():
        if line.startswith("+") and not line.startswith("+++"):
            added += 1
        elif line.startswith("-") and not line.startswith("---"):
            removed += 1
    return added, removed


def parse_args(block):
    args = block.get("args")
    if not args:
        return None
    try:
        return json.loads(args)
    except ValueError:
        return None


def todo_items(block):
    """The checklist of a `todo_write` call, or None for any other tool."""
    name = (block.get("name") or "").lower()
    title = (block.get("title") or "").lower()
    if "todo" not in name and "todo" not in title:
        return None
    args = parse_args(block)
    todos = args.get("todos") if isinstance(args, dict) else None
    if not isinstance(todos, list):
        return None
    items = []
    for todo in todos:
        if isinstance(todo, dict):
            text = todo.get("content") or todo.get("activeForm") or ""
            items.append((todo.get("status") or "pending", str(text)))
    return items


def one_line(text, limit):
    text = " ".join((text or "").split())
    if len(text) > limit:
        return text[: limit - 1] + "…"
    return text


class Rendered:
    """Text of one message plus where its clickable lines are (offsets relative to its start)."""

    def __init__(self):
        self.parts = []
        self.length = 0
        # (offset of line start, offset of line end, block index, kind)
        self.links = []

    def add(self, text):
        self.parts.append(text)
        self.length += len(text)

    def add_line(self, text, block_index=None, kind=None):
        start = self.length
        self.add(text)
        if block_index is not None:
            self.links.append((start, start + len(text), block_index, kind))
        self.add("\n")

    @property
    def text(self):
        return "".join(self.parts)


def route_label(route):
    if not route:
        return "oxi"
    label = route.get("label") or route.get("provider") or "oxi"
    model = route.get("model") or ""
    if model and model.lower() != label.lower():
        label += " · " + model
    if route.get("failover_from"):
        label += " · took over from " + route["failover_from"]
    return label


TABLE_SEPARATOR = re.compile(r"^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$")
FENCE = re.compile(r"^\s*(```|~~~)")


def display_width(text):
    """Columns `text` takes in a monospace font: wide CJK and emoji count 2, accents 0."""
    width = 0
    for char in text:
        if unicodedata.combining(char) or char in "\u200b\u200d\ufe0f":
            continue
        width += 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
    return width


def split_row(line):
    """Cells of a Markdown table row; `|` inside code spans or escaped as `\\|` stays put."""
    cells, current, code, i = [], [], False, 0
    line = line.strip()
    while i < len(line):
        char = line[i]
        if char == "\\" and i + 1 < len(line):
            current.append(line[i:i + 2])
            i += 2
            continue
        if char == "`":
            code = not code
        if char == "|" and not code:
            cells.append("".join(current).strip())
            current = []
        else:
            current.append(char)
        i += 1
    cells.append("".join(current).strip())
    if line.startswith("|"):
        cells = cells[1:]
    if line.endswith("|") and not line.endswith("\\|") and cells:
        cells = cells[:-1]
    return cells


def format_table(rows, separator):
    """Pad the cells so every column lines up in a monospace font."""
    count = max(len(row) for row in rows + [separator])
    rows = [row + [""] * (count - len(row)) for row in rows]
    aligns = []
    for cell in separator + [""] * (count - len(separator)):
        cell = cell.strip()
        if cell.startswith(":") and cell.endswith(":") and len(cell) > 1:
            aligns.append("center")
        elif cell.endswith(":"):
            aligns.append("right")
        else:
            aligns.append("left")
    widths = [max(3, max(display_width(row[c]) for row in rows)) for c in range(count)]

    def pad(text, width, align):
        space = width - display_width(text)
        if align == "right":
            return " " * space + text
        if align == "center":
            return " " * (space // 2) + text + " " * (space - space // 2)
        return text + " " * space

    def line(cells):
        return "| " + " | ".join(pad(cells[c], widths[c], aligns[c]) for c in range(count)) + " |"

    rule = []
    for c in range(count):
        dashes = "-" * widths[c]
        if aligns[c] == "center":
            dashes = ":" + dashes[2:] + ":"
        elif aligns[c] == "right":
            dashes = dashes[1:] + ":"
        rule.append(dashes)
    return [line(rows[0]), "| " + " | ".join(rule) + " |"] + [line(row) for row in rows[1:]]


def align_tables(text):
    """Realign the Markdown tables in `text` (outside code fences) into even columns."""
    if "|" not in text:
        return text
    lines = text.split("\n")
    out, i, fence = [], 0, None
    while i < len(lines):
        current = lines[i]
        match = FENCE.match(current)
        if match:
            marker = match.group(1)
            fence = None if fence == marker else (fence or marker)
        if (
            fence is None
            and "|" in current
            and i + 1 < len(lines)
            and TABLE_SEPARATOR.match(lines[i + 1])
            and "-" in lines[i + 1]
        ):
            rows = [split_row(current)]
            separator = split_row(lines[i + 1])
            j = i + 2
            while j < len(lines) and "|" in lines[j] and lines[j].strip():
                rows.append(split_row(lines[j]))
                j += 1
            indent = current[: len(current) - len(current.lstrip())]
            out.extend(indent + row for row in format_table(rows, separator))
            i = j
            continue
        out.append(current)
        i += 1
    return "\n".join(out)


def render_message(message, options=None):
    options = options or {}
    out = Rendered()
    if message.get("role") == "user":
        header = "Summary of earlier turns" if message.get("summary") else USER_HEADER
        if not header.startswith("▌"):
            header = "▌ " + header
        attachments = message.get("attachments") or []
        # Images get links after the header instead (oxi.py: tool_phantoms).
        if message.get("images"):
            attachments = [a for a in attachments if not a.startswith("image (")]
        if attachments:
            header += "   + " + ", ".join(attachments)
        out.add_line(header, 0, "user")
        text = (message.get("text") or "").strip("\n")
        if text:
            out.add_line(text)
        out.add("\n")
        return out

    out.add_line(ASSISTANT_MARK + route_label(message.get("route")))
    blocks = message.get("blocks") or []
    last_todo = None
    for index, block in enumerate(blocks):
        if block.get("type") == "tool" and todo_items(block) is not None:
            last_todo = index
    output_lines = options.get("tool_output_lines", 4)
    show_thinking = options.get("show_thinking", True)
    # Prose and tool lines are set apart by a blank line; consecutive tools stay together.
    previous = "header"
    for index, block in enumerate(blocks):
        kind = block.get("type")
        if kind == "thinking":
            if not show_thinking or not (block.get("text") or "").strip():
                continue
            if previous == "answer":
                out.add("\n")
            out.add_line(THINKING_MARK + "Thinking: " + one_line(block["text"], 110), index, "thinking")
        elif kind == "answer":
            text = (block.get("text") or "").strip("\n")
            if not text:
                continue
            if previous not in ("header", "answer"):
                out.add("\n")
            out.add_line(align_tables(text))
        elif kind == "tool":
            if previous == "answer":
                out.add("\n")
            render_tool(out, block, index, index == last_todo, output_lines)
        else:
            continue
        previous = kind
    if previous != "header":
        out.add("\n")
    if message.get("streaming"):
        out.add_line(FOOTER_MARK + "working…")
    elif message.get("worked_secs") is not None:
        out.add_line(FOOTER_MARK + "worked " + format_duration(message["worked_secs"]))
    out.add("\n")
    return out


def render_tool(out, block, index, latest_todo, output_lines):
    status = block.get("status") or "completed"
    glyph = STATUS_GLYPHS.get(status, "")
    todos = todo_items(block)
    if todos is not None:
        done = sum(1 for state, _ in todos if state == "completed")
        if not latest_todo:
            out.add_line(
                TOOL_MARK + "Checklist  {}/{} done  {}".format(done, len(todos), glyph),
                index,
                "tool",
            )
            return
        out.add_line(TOOL_MARK + "Checklist  {}/{} done".format(done, len(todos)), index, "tool")
        for state, text in todos:
            out.add_line(TODO_MARKS.get(state, TODO_MARKS["pending"]) + one_line(text, 140))
        return
    name = block.get("name") or "tool"
    title = one_line(block.get("title") or "", 120)
    line = TOOL_MARK + name
    if title and title != name:
        line += "  " + title
    if block.get("diff"):
        added, removed = diff_stats(block["diff"])
        line += "  +{} −{}".format(added, removed)
    if glyph:
        line += "  " + glyph
    out.add_line(line, index, "tool")
    shows_output = block.get("kind") in OUTPUT_KINDS or name in OUTPUT_NAMES
    output = (block.get("output") or "").rstrip()
    if output and output_lines > 0 and (shows_output or status == "failed"):
        lines = output.splitlines()
        for text in lines[-output_lines:]:
            out.add_line(OUTPUT_MARK + one_line(text, 200))


def render_chat_list(chats, current, now):
    """Lines of the chat list; `rows[i]` is the chat id on line i (None = "New chat")."""
    lines = ["＋ New chat"]
    rows = [None]
    for chat in chats:
        if chat.get("approval"):
            mark = "!"
        elif chat.get("running"):
            mark = "◐"
        else:
            mark = " "
        title = one_line(chat.get("title") or "New chat", 80)
        # The age goes first: a narrow column cuts the end of the line.
        lines.append("{}{:>4}  {}".format(mark, format_age(chat.get("modified"), now), title))
        rows.append(chat.get("id"))
    return "\n".join(lines), rows


def tool_details(block):
    """Markdown for the scratch view a tool line opens."""
    parts = ["# {}  {}".format(block.get("name") or "tool", block.get("title") or "")]
    parts.append("Status: {}".format(block.get("status") or ""))
    for location in block.get("locations") or []:
        line = location.get("line")
        parts.append("File: {}{}".format(location.get("path"), ":{}".format(line) if line else ""))
    if block.get("full_output_path"):
        parts.append("Full output: {}".format(block["full_output_path"]))
    args = parse_args(block)
    if args is not None:
        parts.append("\n## Input\n\n```json\n{}\n```".format(json.dumps(args, indent=2, ensure_ascii=False)))
    elif block.get("args"):
        parts.append("\n## Input\n\n```\n{}\n```".format(block["args"]))
    if block.get("diff"):
        note = " (truncated)" if block.get("diff_truncated") else ""
        parts.append("\n## Diff{}\n\n```diff\n{}\n```".format(note, block["diff"].rstrip()))
    if block.get("output"):
        note = " (last part)" if block.get("output_truncated") else ""
        parts.append("\n## Output{}\n\n```\n{}\n```".format(note, block["output"].rstrip()))
    return "\n".join(parts) + "\n"

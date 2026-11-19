import json
import math
import sys
import threading

import cairo
import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk, Pango

APP_CSS = """
.rec-dot {
    min-width: 10px;
    min-height: 10px;
    border-radius: 999px;
    background-color: #e01b24;
}
.rec-dot-paused {
    min-width: 10px;
    min-height: 10px;
    border-radius: 999px;
    background-color: #f5a623;
}
.tool-pill {
    padding: 6px 10px;
}
.swatch-btn {
    padding: 3px;
    min-width: 24px;
    min-height: 24px;
    border-radius: 999px;
}
.swatch-active {
    outline: 2px solid @accent_color;
    outline-offset: 1px;
}
.swatch-dot {
    min-width: 16px;
    min-height: 16px;
    border-radius: 999px;
    border: 1px solid alpha(currentColor, 0.25);
}
.swatch-black { background-color: #000000; }
.swatch-red { background-color: #dc2626; }
.swatch-yellow { background-color: #facc15; }
.swatch-blue { background-color: #2563eb; }
.swatch-green { background-color: #22c55e; }
.swatch-white { background-color: #ffffff; }
"""

TOOL_SVG_TEMPLATES = {
    "pen": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M11.5 2.5l2 2L5.5 12.5l-3 .7.7-3 8.3-7.7z"/></svg>',
    "highlighter": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M10.5 2.5l3 3-6.5 6.5-3.5.5.5-3.5 6.5-6.5zM2.5 14h11"/></svg>',
    "eraser": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M7 13.5h6.5M3.2 9.5l5.3-5.3a1.5 1.5 0 012.1 0l2.2 2.2a1.5 1.5 0 010 2.1L8.5 12.8a1.5 1.5 0 01-2.1 0L3.2 9.6a1.5 1.5 0 010-2.1z"/></svg>',
    "line": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.6" stroke-linecap="round"><path d="M3 13L13 3"/></svg>',
    "rect": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4"><rect x="2.5" y="3.5" width="11" height="9" rx="1.2"/></svg>',
    "ellipse": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4"><circle cx="8" cy="8" r="5.2"/></svg>',
    "arrow": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 13L13 3M7.5 3H13v5.5"/></svg>',
    "redact": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="{color}"><rect x="2.5" y="4" width="11" height="8" rx="1.2"/></svg>',
    "crop": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round"><path d="M4.5 2v9.5H14M2 4.5h9.5V14"/></svg>',
}


def install_css():
    display = Gdk.Display.get_default()
    if display is None:
        return
    provider = Gtk.CssProvider()
    if hasattr(provider, "load_from_string"):
        provider.load_from_string(APP_CSS)
    else:
        provider.load_from_data(APP_CSS.encode("utf-8"))
    Gtk.StyleContext.add_provider_for_display(
        display, provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )


def emit(line: str):
    print(line, flush=True)


def emit_json(obj: dict):
    print(json.dumps(obj, separators=(",", ":")), flush=True)


def start_stdin_reader(app, on_line):
    def reader():
        try:
            for raw in sys.stdin:
                line = raw.rstrip("\r\n")
                GLib.idle_add(on_line, line)
        except Exception:
            pass
        GLib.idle_add(app.quit)

    t = threading.Thread(target=reader, daemon=True)
    t.start()


def make_svg_texture(key: str, color_hex: str):
    svg = TOOL_SVG_TEMPLATES[key].format(color=color_hex).encode("utf-8")
    return Gdk.Texture.new_from_bytes(GLib.Bytes.new(svg))


def build_done(app, duration_secs: int, ext: str, default_path: str):
    install_css()
    state = {"closed_by_host": False, "busy": False, "emitted_discard": False}

    win = Adw.ApplicationWindow(
        application=app,
        title="LX Snipping Tool",
        resizable=False,
    )
    win.set_default_size(500, -1)

    toolbar_view = Adw.ToolbarView()
    header = Adw.HeaderBar()
    toolbar_view.add_top_bar(header)

    body = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=10,
        margin_top=16,
        margin_bottom=18,
        margin_start=20,
        margin_end=20,
    )

    heading = Gtk.Label(
        label="Gravação concluída",
        xalign=0.0,
        halign=Gtk.Align.START,
    )
    heading.add_css_class("title-2")
    body.append(heading)

    mins = duration_secs // 60
    secs = duration_secs % 60
    subtitle = Gtk.Label(
        label=f"Duração: {mins:02d}:{secs:02d}  |  Formato: {ext.upper()}",
        xalign=0.0,
        halign=Gtk.Align.START,
    )
    subtitle.add_css_class("dim-label")
    body.append(subtitle)

    dest_label = Gtk.Label(
        label=f"Destino padrão: {default_path}",
        xalign=0.0,
        halign=Gtk.Align.START,
        wrap=True,
        wrap_mode=Pango.WrapMode.WORD_CHAR,
        max_width_chars=58,
    )
    dest_label.add_css_class("caption")
    dest_label.add_css_class("dim-label")
    body.append(dest_label)

    error_label = Gtk.Label(
        xalign=0.0,
        halign=Gtk.Align.START,
        wrap=True,
        wrap_mode=Pango.WrapMode.WORD_CHAR,
        visible=False,
        max_width_chars=58,
    )
    error_label.add_css_class("caption")
    error_label.add_css_class("error")
    body.append(error_label)

    actions = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=8,
        margin_top=8,
        halign=Gtk.Align.CENTER,
    )

    discard_btn = Gtk.Button(label="Descartar")
    discard_btn.add_css_class("destructive-action")

    save_as_btn = Gtk.Button(label="Salvar como...")

    save_btn = Gtk.Button(label="Salvar")
    save_btn.add_css_class("suggested-action")

    def set_busy(flag: bool):
        state["busy"] = flag
        discard_btn.set_sensitive(not flag)
        save_as_btn.set_sensitive(not flag)
        save_btn.set_sensitive(not flag)

    def on_discard(_btn):
        if state["busy"] or state["emitted_discard"]:
            return
        state["emitted_discard"] = True
        set_busy(True)
        emit("discard")

    def on_save_as(_btn):
        if state["busy"]:
            return
        error_label.set_visible(False)
        set_busy(True)
        emit("save_as")

    def on_save(_btn):
        if state["busy"]:
            return
        error_label.set_visible(False)
        set_busy(True)
        emit("save")

    discard_btn.connect("clicked", on_discard)
    save_as_btn.connect("clicked", on_save_as)
    save_btn.connect("clicked", on_save)

    actions.append(discard_btn)
    actions.append(save_as_btn)
    actions.append(save_btn)
    body.append(actions)

    toolbar_view.set_content(body)
    win.set_content(toolbar_view)
    win.set_default_widget(save_btn)

    key_ctrl = Gtk.EventControllerKey()

    def on_key_pressed(_ctrl, keyval, _keycode, _mods):
        if keyval == Gdk.KEY_Escape and not state["busy"]:
            win.close()
            return True
        return False

    key_ctrl.connect("key-pressed", on_key_pressed)
    win.add_controller(key_ctrl)

    def on_close_request(_win):
        if not state["closed_by_host"] and not state["emitted_discard"]:
            state["emitted_discard"] = True
            emit("discard")
        return False

    win.connect("close-request", on_close_request)

    def on_line(line: str):
        if line == "close":
            state["closed_by_host"] = True
            app.quit()
        elif line == "idle":
            set_busy(False)
        elif line.startswith("error:"):
            set_busy(False)
            msg = line[6:].strip()
            error_label.set_text(msg)
            error_label.set_visible(bool(msg))
        return False

    start_stdin_reader(app, on_line)
    win.present()
    save_btn.grab_focus()


def build_bar(app, elapsed_secs: int, paused: bool, busy: bool):
    install_css()
    state = {"closed_by_host": False}

    win = Gtk.ApplicationWindow(
        application=app,
        title="Gravação pausada" if paused else "Gravando tela",
        resizable=False,
    )

    header = Adw.HeaderBar()
    win.set_titlebar(header)

    row = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=12,
        margin_top=10,
        margin_bottom=10,
        margin_start=16,
        margin_end=16,
        halign=Gtk.Align.CENTER,
        valign=Gtk.Align.CENTER,
    )

    dot = Gtk.Box(width_request=10, height_request=10, valign=Gtk.Align.CENTER)
    dot.add_css_class("rec-dot-paused" if paused else "rec-dot")

    timer_label = Gtk.Label(
        label=f"{elapsed_secs // 60:02d}:{elapsed_secs % 60:02d}",
        valign=Gtk.Align.CENTER,
    )
    timer_label.add_css_class("title-4")
    timer_label.add_css_class("numeric")

    pause_icon = Gtk.Image.new_from_icon_name(
        "media-playback-start-symbolic" if paused else "media-playback-pause-symbolic"
    )
    pause_label = Gtk.Label(label="Retomar" if paused else "Pausar")
    pause_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
    pause_box.append(pause_icon)
    pause_box.append(pause_label)
    pause_btn = Gtk.Button(child=pause_box, sensitive=not busy)

    stop_icon = Gtk.Image.new_from_icon_name("media-playback-stop-symbolic")
    stop_label = Gtk.Label(label="Parar")
    stop_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
    stop_box.append(stop_icon)
    stop_box.append(stop_label)
    stop_btn = Gtk.Button(child=stop_box, sensitive=not busy)
    stop_btn.add_css_class("destructive-action")

    pause_btn.connect("clicked", lambda _b: emit("toggle_pause"))
    stop_btn.connect("clicked", lambda _b: emit("stop"))

    row.append(dot)
    row.append(timer_label)
    row.append(pause_btn)
    row.append(stop_btn)

    win.set_child(row)

    def on_close_request(_win):
        if not state["closed_by_host"]:
            emit("closed")
        return False

    win.connect("close-request", on_close_request)

    def apply_state(el: int, is_paused: bool, is_busy: bool):
        win.set_title("Gravação pausada" if is_paused else "Gravando tela")
        dot.remove_css_class("rec-dot")
        dot.remove_css_class("rec-dot-paused")
        dot.add_css_class("rec-dot-paused" if is_paused else "rec-dot")
        timer_label.set_text(f"{el // 60:02d}:{el % 60:02d}")
        pause_icon.set_from_icon_name(
            "media-playback-start-symbolic" if is_paused else "media-playback-pause-symbolic"
        )
        pause_label.set_text("Retomar" if is_paused else "Pausar")
        pause_btn.set_sensitive(not is_busy)
        stop_btn.set_sensitive(not is_busy)

    def on_line(line: str):
        if line == "close":
            state["closed_by_host"] = True
            app.quit()
        elif line.startswith("state:"):
            parts = line.split(":")
            if len(parts) == 4:
                try:
                    el = int(parts[1])
                    is_paused = parts[2] == "1"
                    is_busy = parts[3] == "1"
                    apply_state(el, is_paused, is_busy)
                except ValueError:
                    pass
        return False

    start_stdin_reader(app, on_line)
    win.present()


def build_toast(app):
    install_css()
    state = {"remaining": 2, "finished": False, "cancelled": False}

    win = Gtk.ApplicationWindow(
        application=app,
        title="Iniciando gravação",
        resizable=False,
    )

    header = Adw.HeaderBar()
    win.set_titlebar(header)

    body = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=6,
        margin_top=12,
        margin_bottom=12,
        margin_start=18,
        margin_end=18,
        halign=Gtk.Align.CENTER,
        valign=Gtk.Align.CENTER,
    )

    top_row = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=10,
        halign=Gtk.Align.CENTER,
        valign=Gtk.Align.CENTER,
    )
    dot = Gtk.Box(width_request=10, height_request=10, valign=Gtk.Align.CENTER)
    dot.add_css_class("rec-dot")
    countdown_label = Gtk.Label(
        label="Iniciando a gravação em 2 s",
        valign=Gtk.Align.CENTER,
    )
    countdown_label.add_css_class("title-4")
    countdown_label.add_css_class("numeric")
    top_row.append(dot)
    top_row.append(countdown_label)

    hint_label = Gtk.Label(
        label="Barra superior: clique do meio pausa • duplo clique para • clique abre o menu",
        halign=Gtk.Align.CENTER,
    )
    hint_label.add_css_class("caption")
    hint_label.add_css_class("dim-label")

    body.append(top_row)
    body.append(hint_label)
    win.set_child(body)

    key_ctrl = Gtk.EventControllerKey()

    def on_key_pressed(_ctrl, keyval, _keycode, _mods):
        if keyval == Gdk.KEY_Escape:
            win.close()
            return True
        return False

    key_ctrl.connect("key-pressed", on_key_pressed)
    win.add_controller(key_ctrl)

    def on_close_request(_win):
        if not state["finished"] and not state["cancelled"]:
            state["cancelled"] = True
            emit("cancel")
        return False

    win.connect("close-request", on_close_request)

    def finish_and_close():
        if state["cancelled"]:
            return GLib.SOURCE_REMOVE
        state["finished"] = True
        emit("ready")
        win.set_visible(False)
        app.quit()
        return GLib.SOURCE_REMOVE

    def on_tick():
        if state["cancelled"]:
            return GLib.SOURCE_REMOVE
        state["remaining"] -= 1
        countdown_label.set_text(f"Iniciando a gravação em {state['remaining']} s")
        if state["remaining"] <= 0:
            GLib.timeout_add(250, finish_and_close)
            return GLib.SOURCE_REMOVE
        return GLib.SOURCE_CONTINUE

    GLib.timeout_add(1000, on_tick)
    win.present()


def build_delay(app, remaining_secs: int):
    install_css()
    state = {"closed_by_host": False, "emitted_cancel": False}

    win = Gtk.ApplicationWindow(
        application=app,
        title="LX Snipping Tool",
        resizable=False,
    )
    win.set_default_size(280, -1)

    header = Adw.HeaderBar()
    win.set_titlebar(header)

    body = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=12,
        margin_top=12,
        margin_bottom=12,
        margin_start=20,
        margin_end=20,
        halign=Gtk.Align.CENTER,
    )

    countdown_label = Gtk.Label(
        label=f"Capturando em {remaining_secs}s...",
        halign=Gtk.Align.CENTER,
    )
    countdown_label.add_css_class("title-3")
    countdown_label.add_css_class("numeric")
    body.append(countdown_label)

    cancel_btn = Gtk.Button(label="Cancelar", halign=Gtk.Align.CENTER)

    def on_cancel(_btn=None):
        if not state["emitted_cancel"]:
            state["emitted_cancel"] = True
            emit("cancel")

    cancel_btn.connect("clicked", on_cancel)
    body.append(cancel_btn)

    win.set_child(body)

    key_ctrl = Gtk.EventControllerKey()

    def on_key_pressed(_ctrl, keyval, _keycode, _mods):
        if keyval == Gdk.KEY_Escape:
            win.close()
            return True
        return False

    key_ctrl.connect("key-pressed", on_key_pressed)
    win.add_controller(key_ctrl)

    def on_close_request(_win):
        if not state["closed_by_host"] and not state["emitted_cancel"]:
            state["emitted_cancel"] = True
            emit("cancel")
        return False

    win.connect("close-request", on_close_request)

    def on_line(line: str):
        if line == "close":
            state["closed_by_host"] = True
            app.quit()
        elif line.startswith("tick:"):
            try:
                secs = int(line[5:])
                countdown_label.set_text(f"Capturando em {secs}s...")
            except ValueError:
                pass
        return False

    start_stdin_reader(app, on_line)
    win.present()


def build_error(app, msg: str):
    install_css()
    win = Adw.ApplicationWindow(
        application=app,
        title="LX Snipping Tool",
        resizable=False,
    )
    win.set_default_size(480, -1)

    toolbar_view = Adw.ToolbarView()
    header = Adw.HeaderBar()
    toolbar_view.add_top_bar(header)

    body = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=12,
        margin_top=16,
        margin_bottom=18,
        margin_start=20,
        margin_end=20,
    )

    heading = Gtk.Label(
        label="Erro",
        xalign=0.0,
        halign=Gtk.Align.START,
    )
    heading.add_css_class("title-2")
    body.append(heading)

    msg_label = Gtk.Label(
        label=msg,
        xalign=0.0,
        halign=Gtk.Align.START,
        wrap=True,
        wrap_mode=Pango.WrapMode.WORD_CHAR,
        max_width_chars=58,
    )
    body.append(msg_label)

    close_btn = Gtk.Button(label="Fechar", halign=Gtk.Align.CENTER)
    close_btn.add_css_class("suggested-action")
    close_btn.connect("clicked", lambda _b: win.close())
    body.append(close_btn)

    toolbar_view.set_content(body)
    win.set_content(toolbar_view)
    win.set_default_widget(close_btn)

    key_ctrl = Gtk.EventControllerKey()

    def on_key_pressed(_ctrl, keyval, _keycode, _mods):
        if keyval == Gdk.KEY_Escape:
            win.close()
            return True
        return False

    key_ctrl.connect("key-pressed", on_key_pressed)
    win.add_controller(key_ctrl)

    win.present()
    close_btn.grab_focus()


def build_history(app):
    install_css()
    state = {"closed_by_host": False, "emitted_close": False}

    win = Adw.ApplicationWindow(
        application=app,
        title="Histórico — LX Snipping Tool",
        default_width=720,
        default_height=560,
    )

    toolbar_view = Adw.ToolbarView()
    header = Adw.HeaderBar()
    toolbar_view.add_top_bar(header)

    root = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=12,
        margin_top=16,
        margin_bottom=16,
        margin_start=16,
        margin_end=16,
    )

    top_row = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=12,
        halign=Gtk.Align.CENTER,
        valign=Gtk.Align.CENTER,
    )
    title_label = Gtk.Label(label="Snip History (0)")
    title_label.add_css_class("title-3")
    refresh_btn = Gtk.Button(label="Refresh")
    refresh_btn.connect("clicked", lambda _b: emit_json({"cmd": "refresh"}))
    top_row.append(title_label)
    top_row.append(refresh_btn)
    root.append(top_row)

    status_label = Gtk.Label(
        xalign=0.0,
        halign=Gtk.Align.CENTER,
        visible=False,
        wrap=True,
    )
    status_label.add_css_class("caption")
    status_label.add_css_class("error")
    root.append(status_label)

    scrolled = Gtk.ScrolledWindow(vexpand=True, hexpand=True)
    list_box = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=10,
        margin_top=4,
        margin_bottom=4,
        margin_start=4,
        margin_end=4,
    )
    scrolled.set_child(list_box)
    root.append(scrolled)

    toolbar_view.set_content(root)
    win.set_content(toolbar_view)

    def clear_box(box):
        child = box.get_first_child()
        while child is not None:
            nxt = child.get_next_sibling()
            box.remove(child)
            child = nxt

    def rebuild(payload: dict):
        items = payload.get("items", [])
        status = payload.get("status")
        title_label.set_text(f"Snip History ({len(items)})")
        if status:
            status_label.set_text(status)
            status_label.set_visible(True)
        else:
            status_label.set_visible(False)

        clear_box(list_box)
        if not items:
            empty = Gtk.Label(
                label="No snips saved yet.",
                halign=Gtk.Align.CENTER,
                margin_top=24,
            )
            empty.add_css_class("dim-label")
            list_box.append(empty)
            return

        for item in items:
            path = item["path"]
            name = item["name"]
            kb = item["kb"]

            card = Gtk.Box(
                orientation=Gtk.Orientation.HORIZONTAL,
                spacing=14,
                margin_start=2,
                margin_end=2,
            )
            card.add_css_class("card")

            inner = Gtk.Box(
                orientation=Gtk.Orientation.HORIZONTAL,
                spacing=14,
                margin_top=10,
                margin_bottom=10,
                margin_start=12,
                margin_end=12,
                hexpand=True,
            )

            pic = Gtk.Picture.new_for_filename(path)
            pic.set_size_request(160, 100)
            pic.set_content_fit(Gtk.ContentFit.CONTAIN)
            pic.set_can_shrink(True)

            meta = Gtk.Box(
                orientation=Gtk.Orientation.VERTICAL,
                spacing=6,
                hexpand=True,
                valign=Gtk.Align.CENTER,
            )
            name_lbl = Gtk.Label(
                label=name,
                xalign=0.0,
                halign=Gtk.Align.START,
                ellipsize=Pango.EllipsizeMode.MIDDLE,
            )
            name_lbl.add_css_class("heading")
            size_lbl = Gtk.Label(
                label=f"{kb:.1f} KiB",
                xalign=0.0,
                halign=Gtk.Align.START,
            )
            size_lbl.add_css_class("caption")
            size_lbl.add_css_class("dim-label")

            btn_row = Gtk.Box(
                orientation=Gtk.Orientation.HORIZONTAL,
                spacing=8,
                halign=Gtk.Align.CENTER,
                margin_top=4,
            )
            copy_btn = Gtk.Button(label="Copy")
            copy_btn.add_css_class("suggested-action")
            copy_btn.connect("clicked", lambda _b, p=path: emit_json({"cmd": "copy", "path": p}))

            edit_btn = Gtk.Button(label="Edit")
            edit_btn.connect("clicked", lambda _b, p=path: emit_json({"cmd": "edit", "path": p}))

            del_btn = Gtk.Button(label="Delete")
            del_btn.add_css_class("destructive-action")
            del_btn.connect("clicked", lambda _b, p=path: emit_json({"cmd": "delete", "path": p}))

            btn_row.append(copy_btn)
            btn_row.append(edit_btn)
            btn_row.append(del_btn)

            meta.append(name_lbl)
            meta.append(size_lbl)
            meta.append(btn_row)

            inner.append(pic)
            inner.append(meta)
            card.append(inner)
            list_box.append(card)

    def send_close_once():
        if not state["closed_by_host"] and not state["emitted_close"]:
            state["emitted_close"] = True
            emit_json({"cmd": "close"})

    key_ctrl = Gtk.EventControllerKey()

    def on_key_pressed(_ctrl, keyval, _keycode, _mods):
        if keyval == Gdk.KEY_Escape:
            win.close()
            return True
        return False

    key_ctrl.connect("key-pressed", on_key_pressed)
    win.add_controller(key_ctrl)

    def on_close_request(_win):
        send_close_once()
        return False

    win.connect("close-request", on_close_request)

    def on_line(line: str):
        if line == "close":
            state["closed_by_host"] = True
            app.quit()
        elif line.startswith("items:"):
            try:
                rebuild(json.loads(line[6:]))
            except Exception:
                pass
        return False

    start_stdin_reader(app, on_line)
    win.present()


def build_editor(app, base_png_path: str, img_w: int, img_h: int, initial_dark: bool):
    install_css()
    style_mgr = Adw.StyleManager.get_default()
    style_mgr.set_color_scheme(
        Adw.ColorScheme.FORCE_DARK if initial_dark else Adw.ColorScheme.FORCE_LIGHT
    )

    base_surface = cairo.ImageSurface.create_from_png(base_png_path)

    state = {
        "closed_by_host": False,
        "emitted_close": False,
        "dark": initial_dark,
        "tool": "pen",
        "color": {"r": 220, "g": 38, "b": 38, "a": 255},
        "width": 4.0,
        "ops": [],
        "crop_rect": None,
        "dragging": False,
        "drag_start": None,
        "drag_current": None,
        "points": [],
    }

    win = Adw.ApplicationWindow(
        application=app,
        title="LX Snipping Tool",
        default_width=980,
        default_height=680,
    )

    toolbar_view = Adw.ToolbarView()
    header = Adw.HeaderBar()
    toolbar_view.add_top_bar(header)

    root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=0)

    # Centered action bar matching Iced editor layout
    action_bar = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=6,
        margin_top=6,
        margin_bottom=6,
        margin_start=14,
        margin_end=14,
        halign=Gtk.Align.CENTER,
        valign=Gtk.Align.CENTER,
    )

    new_btn = Gtk.Button(label="+ New")
    new_btn.connect("clicked", lambda _b: emit_json({"cmd": "new"}))

    undo_btn = Gtk.Button()
    undo_btn.set_icon_name("edit-undo-symbolic")
    undo_btn.set_tooltip_text("Undo (Ctrl+Z)")
    undo_btn.set_sensitive(False)
    undo_btn.connect("clicked", lambda _b: emit_json({"cmd": "undo"}))

    redo_btn = Gtk.Button()
    redo_btn.set_icon_name("edit-redo-symbolic")
    redo_btn.set_tooltip_text("Redo (Ctrl+Y)")
    redo_btn.set_sensitive(False)
    redo_btn.connect("clicked", lambda _b: emit_json({"cmd": "redo"}))

    save_btn = Gtk.Button(label="Save")
    save_btn.connect("clicked", lambda _b: emit_json({"cmd": "save"}))

    copy_btn = Gtk.Button(label="Copy")
    copy_btn.add_css_class("suggested-action")
    copy_btn.connect("clicked", lambda _b: emit_json({"cmd": "copy"}))

    history_btn = Gtk.Button(label="History")
    history_btn.connect("clicked", lambda _b: emit_json({"cmd": "history"}))

    theme_btn = Gtk.Button(label="Light" if initial_dark else "Dark")

    action_bar.append(new_btn)
    action_bar.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
    action_bar.append(undo_btn)
    action_bar.append(redo_btn)
    action_bar.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
    action_bar.append(save_btn)
    action_bar.append(copy_btn)
    action_bar.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
    action_bar.append(history_btn)
    action_bar.append(theme_btn)
    root.append(action_bar)

    # Centered floating tool pill
    sub_row = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=8,
        margin_top=6,
        margin_bottom=8,
        margin_start=12,
        margin_end=12,
        halign=Gtk.Align.CENTER,
        valign=Gtk.Align.CENTER,
    )

    tools_card = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=4,
        valign=Gtk.Align.CENTER,
    )
    tools_card.add_css_class("card")
    tools_card.add_css_class("tool-pill")

    tool_defs = [
        ("pen", "Pen"),
        ("highlighter", "Highlighter"),
        ("eraser", "Eraser"),
        None,
        ("line", "Line"),
        ("rect", "Rectangle"),
        ("ellipse", "Ellipse"),
        ("arrow", "Arrow"),
        None,
        ("redact", "Redact"),
        ("crop", "Crop"),
    ]

    tool_images = {}
    first_tool_btn = None

    def current_icon_hex():
        return "#f4f4f6" if state["dark"] else "#1a1a1c"

    def refresh_tool_icons():
        hex_color = current_icon_hex()
        for key, img in tool_images.items():
            img.set_from_paintable(make_svg_texture(key, hex_color))

    def on_tool_toggled(btn, key):
        if btn.get_active():
            state["tool"] = key

    for entry in tool_defs:
        if entry is None:
            tools_card.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
            continue
        key, tip = entry
        tex = make_svg_texture(key, current_icon_hex())
        img = Gtk.Image.new_from_paintable(tex)
        img.set_pixel_size(16)
        tool_images[key] = img
        btn = Gtk.ToggleButton(child=img, tooltip_text=tip)
        btn.add_css_class("flat")
        if first_tool_btn is None:
            first_tool_btn = btn
            btn.set_active(True)
        else:
            btn.set_group(first_tool_btn)
        btn.connect("toggled", on_tool_toggled, key)
        tools_card.append(btn)

    tools_card.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))

    swatch_defs = [
        ("swatch-black", {"r": 0, "g": 0, "b": 0, "a": 255}),
        ("swatch-red", {"r": 220, "g": 38, "b": 38, "a": 255}),
        ("swatch-yellow", {"r": 250, "g": 204, "b": 21, "a": 255}),
        ("swatch-blue", {"r": 37, "g": 99, "b": 235, "a": 255}),
        ("swatch-green", {"r": 34, "g": 197, "b": 94, "a": 255}),
        ("swatch-white", {"r": 255, "g": 255, "b": 255, "a": 255}),
    ]
    swatch_buttons = []

    def select_swatch(idx):
        state["color"] = dict(swatch_defs[idx][1])
        for i, b in enumerate(swatch_buttons):
            if i == idx:
                b.add_css_class("swatch-active")
            else:
                b.remove_css_class("swatch-active")

    for idx, (cls, _col) in enumerate(swatch_defs):
        dot = Gtk.Box(halign=Gtk.Align.CENTER, valign=Gtk.Align.CENTER)
        dot.add_css_class("swatch-dot")
        dot.add_css_class(cls)
        sbtn = Gtk.Button(child=dot)
        sbtn.add_css_class("flat")
        sbtn.add_css_class("swatch-btn")
        if idx == 1:
            sbtn.add_css_class("swatch-active")
        sbtn.connect("clicked", lambda _b, i=idx: select_swatch(i))
        swatch_buttons.append(sbtn)
        tools_card.append(sbtn)

    tools_card.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))

    width_defs = [("S", 2.0), ("M", 4.0), ("L", 8.0)]
    first_width_btn = None

    def on_width_toggled(btn, w_val):
        if btn.get_active():
            state["width"] = w_val

    for label, w_val in width_defs:
        wbtn = Gtk.ToggleButton(label=label)
        wbtn.add_css_class("flat")
        if first_width_btn is None:
            first_width_btn = wbtn
        else:
            wbtn.set_group(first_width_btn)
        if abs(w_val - 4.0) < 0.1:
            wbtn.set_active(True)
        wbtn.connect("toggled", on_width_toggled, w_val)
        tools_card.append(wbtn)

    reset_crop_btn = Gtk.Button(label="Reset Crop", visible=False)
    reset_crop_btn.connect("clicked", lambda _b: emit_json({"cmd": "set_crop", "crop": None}))

    sub_row.append(tools_card)
    sub_row.append(reset_crop_btn)
    root.append(sub_row)

    def on_toggle_theme(_btn):
        state["dark"] = not state["dark"]
        style_mgr.set_color_scheme(
            Adw.ColorScheme.FORCE_DARK if state["dark"] else Adw.ColorScheme.FORCE_LIGHT
        )
        theme_btn.set_label("Light" if state["dark"] else "Dark")
        refresh_tool_icons()
        area.queue_draw()

    theme_btn.connect("clicked", on_toggle_theme)

    # Drawing canvas
    area = Gtk.DrawingArea(hexpand=True, vexpand=True)
    root.append(area)

    # Bottom status bar
    status_row = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        margin_top=6,
        margin_bottom=6,
        margin_start=14,
        margin_end=14,
    )
    status_label = Gtk.Label(label="Ready", xalign=0.0, hexpand=True, halign=Gtk.Align.START)
    status_label.add_css_class("caption")
    status_label.add_css_class("dim-label")
    dim_label = Gtk.Label(label=f"{img_w} × {img_h} px", halign=Gtk.Align.END)
    dim_label.add_css_class("caption")
    dim_label.add_css_class("dim-label")
    status_row.append(status_label)
    status_row.append(dim_label)
    root.append(status_row)

    toolbar_view.set_content(root)
    win.set_content(toolbar_view)

    def fit_rect(w: float, h: float):
        pad = 24.0
        avail_w = max(w - pad, 1.0)
        avail_h = max(h - pad, 1.0)
        scale = min(avail_w / max(img_w, 1), avail_h / max(img_h, 1), 1.0)
        draw_w = img_w * scale
        draw_h = img_h * scale
        return ((w - draw_w) * 0.5, (h - draw_h) * 0.5, draw_w, draw_h, scale)

    def to_image_point(w: float, h: float, cx: float, cy: float):
        fx, fy, fw, fh, _ = fit_rect(w, h)
        ix = max(0.0, min(float(img_w), ((cx - fx) / max(fw, 1.0)) * img_w))
        iy = max(0.0, min(float(img_h), ((cy - fy) / max(fh, 1.0)) * img_h))
        return {"x": ix, "y": iy}

    def set_rgba(cr, col: dict, alpha_mul: float = 1.0):
        a = max(0.0, min(1.0, (col["a"] / 255.0) * alpha_mul))
        cr.set_source_rgba(col["r"] / 255.0, col["g"] / 255.0, col["b"] / 255.0, a)

    def draw_polyline(cr, fx, fy, scale, pts, col, width, alpha_mul=1.0):
        if not pts:
            return
        set_rgba(cr, col, alpha_mul)
        if len(pts) == 1:
            r = max(width * scale * 0.5, 1.0)
            cr.arc(fx + pts[0]["x"] * scale, fy + pts[0]["y"] * scale, r, 0, math.tau)
            cr.fill()
            return
        cr.set_line_width(max(width * scale, 1.0))
        cr.set_line_cap(cairo.LINE_CAP_ROUND)
        cr.set_line_join(cairo.LINE_JOIN_ROUND)
        cr.move_to(fx + pts[0]["x"] * scale, fy + pts[0]["y"] * scale)
        for p in pts[1:]:
            cr.line_to(fx + p["x"] * scale, fy + p["y"] * scale)
        cr.stroke()

    def draw_op(cr, fx, fy, scale, op: dict):
        kind_key, data = next(iter(op.items()))
        if kind_key == "Pen":
            draw_polyline(cr, fx, fy, scale, data["points"], data["color"], data["width"], 1.0)
        elif kind_key == "Highlighter":
            draw_polyline(
                cr,
                fx,
                fy,
                scale,
                data["points"],
                data["color"],
                data["width"],
                data["opacity"],
            )
        elif kind_key == "Shape":
            ax = fx + data["start"]["x"] * scale
            ay = fy + data["start"]["y"] * scale
            bx = fx + data["end"]["x"] * scale
            by = fy + data["end"]["y"] * scale
            set_rgba(cr, data["color"], 1.0)
            cr.set_line_width(max(data["width"] * scale, 1.0))
            cr.set_line_cap(cairo.LINE_CAP_ROUND)
            cr.set_line_join(cairo.LINE_JOIN_ROUND)
            skind = data["kind"]
            if skind == "Rectangle":
                cr.rectangle(min(ax, bx), min(ay, by), abs(bx - ax), abs(by - ay))
                cr.stroke()
            elif skind == "Ellipse":
                rx = max(abs(bx - ax) * 0.5, 0.5)
                ry = max(abs(by - ay) * 0.5, 0.5)
                cx = (ax + bx) * 0.5
                cy = (ay + by) * 0.5
                cr.save()
                cr.translate(cx, cy)
                cr.scale(rx, ry)
                cr.arc(0.0, 0.0, 1.0, 0.0, math.tau)
                cr.restore()
                cr.stroke()
            elif skind == "Line":
                cr.move_to(ax, ay)
                cr.line_to(bx, by)
                cr.stroke()
            elif skind == "Arrow":
                cr.move_to(ax, ay)
                cr.line_to(bx, by)
                cr.stroke()
                dx = bx - ax
                dy = by - ay
                length = math.hypot(dx, dy)
                if length > 1e-3:
                    head_len = max(8.0, min(28.0, data["width"] * scale * 3.5))
                    ang = math.atan2(dy, dx)
                    spread = 0.45
                    cr.move_to(bx, by)
                    cr.line_to(
                        bx - head_len * math.cos(ang - spread),
                        by - head_len * math.sin(ang - spread),
                    )
                    cr.stroke()
                    cr.move_to(bx, by)
                    cr.line_to(
                        bx - head_len * math.cos(ang + spread),
                        by - head_len * math.sin(ang + spread),
                    )
                    cr.stroke()
        elif kind_key == "RedactBox":
            r = data["rect"]
            rx = min(r["x"], r["x"] + r["width"])
            ry = min(r["y"], r["y"] + r["height"])
            rw = abs(r["width"])
            rh = abs(r["height"])
            cr.set_source_rgb(0.0, 0.0, 0.0)
            cr.rectangle(fx + rx * scale, fy + ry * scale, rw * scale, rh * scale)
            cr.fill()

    def on_draw(_area, cr, w, h):
        fx, fy, fw, fh, scale = fit_rect(float(w), float(h))

        # Subtle backdrop + border around image fit area
        cr.save()
        cr.translate(fx, fy)
        cr.scale(scale, scale)
        cr.set_source_surface(base_surface, 0.0, 0.0)
        cr.paint()
        cr.restore()

        cr.set_source_rgba(0.5, 0.5, 0.55, 0.35)
        cr.set_line_width(1.0)
        cr.rectangle(fx, fy, fw, fh)
        cr.stroke()

        for op in state["ops"]:
            draw_op(cr, fx, fy, scale, op)

        # In-progress live preview
        tool = state["tool"]
        if state["dragging"] and state["drag_start"] and state["drag_current"]:
            a = state["drag_start"]
            b = state["drag_current"]
            if tool == "pen":
                draw_polyline(
                    cr, fx, fy, scale, state["points"], state["color"], state["width"], 1.0
                )
            elif tool == "highlighter":
                draw_polyline(
                    cr,
                    fx,
                    fy,
                    scale,
                    state["points"],
                    state["color"],
                    max(state["width"] * 3.0, 12.0),
                    0.4,
                )
            elif tool in ("line", "rect", "ellipse", "arrow"):
                kind_map = {
                    "line": "Line",
                    "rect": "Rectangle",
                    "ellipse": "Ellipse",
                    "arrow": "Arrow",
                }
                draw_op(
                    cr,
                    fx,
                    fy,
                    scale,
                    {
                        "Shape": {
                            "kind": kind_map[tool],
                            "start": a,
                            "end": b,
                            "color": state["color"],
                            "width": state["width"],
                        }
                    },
                )
            elif tool == "redact":
                rx = min(a["x"], b["x"])
                ry = min(a["y"], b["y"])
                rw = abs(b["x"] - a["x"])
                rh = abs(b["y"] - a["y"])
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.85)
                cr.rectangle(fx + rx * scale, fy + ry * scale, rw * scale, rh * scale)
                cr.fill()
            elif tool == "crop":
                rx = fx + min(a["x"], b["x"]) * scale
                ry = fy + min(a["y"], b["y"]) * scale
                rw = abs(b["x"] - a["x"]) * scale
                rh = abs(b["y"] - a["y"]) * scale
                cr.set_source_rgba(0.0, 0.47, 0.83, 0.18)
                cr.rectangle(rx, ry, rw, rh)
                cr.fill()
                cr.set_source_rgb(0.0, 0.47, 0.83)
                cr.set_line_width(2.0)
                cr.rectangle(rx, ry, rw, rh)
                cr.stroke()

        # Committed crop dim mask
        if state["crop_rect"]:
            r = state["crop_rect"]
            cx = fx + r["x"] * scale
            cy = fy + r["y"] * scale
            cw = r["width"] * scale
            ch = r["height"] * scale
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.48)
            cr.rectangle(fx, fy, fw, cy - fy)
            cr.fill()
            cr.rectangle(fx, cy + ch, fw, (fy + fh) - (cy + ch))
            cr.fill()
            cr.rectangle(fx, cy, cx - fx, ch)
            cr.fill()
            cr.rectangle(cx + cw, cy, (fx + fw) - (cx + cw), ch)
            cr.fill()
            cr.set_source_rgb(1.0, 1.0, 1.0)
            cr.set_line_width(1.5)
            cr.rectangle(cx, cy, cw, ch)
            cr.stroke()

    area.set_draw_func(on_draw)

    drag = Gtk.GestureDrag()
    drag_origin = {"x": 0.0, "y": 0.0}

    def on_drag_begin(_g, sx, sy):
        drag_origin["x"] = sx
        drag_origin["y"] = sy
        pt = to_image_point(area.get_width(), area.get_height(), sx, sy)
        state["dragging"] = True
        state["drag_start"] = pt
        state["drag_current"] = pt
        if state["tool"] in ("pen", "highlighter"):
            state["points"] = [pt]
        elif state["tool"] == "eraser":
            emit_json(
                {
                    "cmd": "erase",
                    "x": pt["x"],
                    "y": pt["y"],
                    "radius": max(state["width"] * 2.0, 10.0),
                }
            )
        area.queue_draw()

    def on_drag_update(_g, ox, oy):
        if not state["dragging"]:
            return
        pt = to_image_point(
            area.get_width(),
            area.get_height(),
            drag_origin["x"] + ox,
            drag_origin["y"] + oy,
        )
        state["drag_current"] = pt
        if state["tool"] in ("pen", "highlighter"):
            state["points"].append(pt)
        elif state["tool"] == "eraser":
            emit_json(
                {
                    "cmd": "erase",
                    "x": pt["x"],
                    "y": pt["y"],
                    "radius": max(state["width"] * 2.0, 10.0),
                }
            )
        area.queue_draw()

    def on_drag_end(_g, ox, oy):
        if not state["dragging"]:
            return
        pt = to_image_point(
            area.get_width(),
            area.get_height(),
            drag_origin["x"] + ox,
            drag_origin["y"] + oy,
        )
        a = state["drag_start"]
        state["dragging"] = False
        state["drag_start"] = None
        state["drag_current"] = None
        tool = state["tool"]

        if tool == "pen" and state["points"]:
            pts = list(state["points"])
            state["points"] = []
            emit_json(
                {
                    "cmd": "push_op",
                    "op": {
                        "Pen": {
                            "points": pts,
                            "color": state["color"],
                            "width": state["width"],
                        }
                    },
                }
            )
        elif tool == "highlighter" and state["points"]:
            pts = list(state["points"])
            state["points"] = []
            emit_json(
                {
                    "cmd": "push_op",
                    "op": {
                        "Highlighter": {
                            "points": pts,
                            "color": state["color"],
                            "width": max(state["width"] * 3.0, 12.0),
                            "opacity": 0.4,
                        }
                    },
                }
            )
        elif tool in ("line", "rect", "ellipse", "arrow") and a is not None:
            kind_map = {
                "line": "Line",
                "rect": "Rectangle",
                "ellipse": "Ellipse",
                "arrow": "Arrow",
            }
            emit_json(
                {
                    "cmd": "push_op",
                    "op": {
                        "Shape": {
                            "kind": kind_map[tool],
                            "start": a,
                            "end": pt,
                            "color": state["color"],
                            "width": state["width"],
                        }
                    },
                }
            )
        elif tool == "redact" and a is not None:
            rx = min(a["x"], pt["x"])
            ry = min(a["y"], pt["y"])
            rw = abs(pt["x"] - a["x"])
            rh = abs(pt["y"] - a["y"])
            if rw >= 2.0 and rh >= 2.0:
                emit_json(
                    {
                        "cmd": "push_op",
                        "op": {
                            "RedactBox": {
                                "rect": {"x": rx, "y": ry, "width": rw, "height": rh}
                            }
                        },
                    }
                )
        elif tool == "crop" and a is not None:
            rx = min(a["x"], pt["x"])
            ry = min(a["y"], pt["y"])
            rw = abs(pt["x"] - a["x"])
            rh = abs(pt["y"] - a["y"])
            if rw >= 4.0 and rh >= 4.0:
                emit_json(
                    {
                        "cmd": "set_crop",
                        "crop": {"x": rx, "y": ry, "width": rw, "height": rh},
                    }
                )
        area.queue_draw()

    drag.connect("drag-begin", on_drag_begin)
    drag.connect("drag-update", on_drag_update)
    drag.connect("drag-end", on_drag_end)
    area.add_controller(drag)

    def send_close_once():
        if not state["closed_by_host"] and not state["emitted_close"]:
            state["emitted_close"] = True
            emit_json({"cmd": "close"})

    key_ctrl = Gtk.EventControllerKey()

    def on_key_pressed(_ctrl, keyval, _keycode, mods):
        ctrl = bool(mods & Gdk.ModifierType.CONTROL_MASK)
        shift = bool(mods & Gdk.ModifierType.SHIFT_MASK)
        if keyval == Gdk.KEY_Escape:
            win.close()
            return True
        if ctrl and keyval in (Gdk.KEY_z, Gdk.KEY_Z):
            emit_json({"cmd": "redo" if shift else "undo"})
            return True
        if ctrl and keyval in (Gdk.KEY_y, Gdk.KEY_Y):
            emit_json({"cmd": "redo"})
            return True
        if ctrl and keyval in (Gdk.KEY_c, Gdk.KEY_C):
            emit_json({"cmd": "copy"})
            return True
        if ctrl and keyval in (Gdk.KEY_s, Gdk.KEY_S):
            emit_json({"cmd": "save"})
            return True
        return False

    key_ctrl.connect("key-pressed", on_key_pressed)
    win.add_controller(key_ctrl)

    def on_close_request(_win):
        send_close_once()
        return False

    win.connect("close-request", on_close_request)

    def on_line(line: str):
        if line == "close":
            state["closed_by_host"] = True
            app.quit()
        elif line.startswith("state:"):
            try:
                payload = json.loads(line[6:])
                state["ops"] = payload.get("ops", [])
                state["crop_rect"] = payload.get("crop_rect")
                undo_btn.set_sensitive(bool(payload.get("can_undo", False)))
                redo_btn.set_sensitive(bool(payload.get("can_redo", False)))
                reset_crop_btn.set_visible(state["crop_rect"] is not None)
                status = payload.get("status")
                status_label.set_text(status if status else "Ready")
                area.queue_draw()
            except Exception:
                pass
        return False

    start_stdin_reader(app, on_line)
    win.present()


def main():
    mode = sys.argv[1]
    app = Adw.Application(
        application_id="io.github.lx_snippingtool.SnippingTool.NativeUi",
        flags=Gio.ApplicationFlags.NON_UNIQUE,
    )

    def on_activate(application):
        if mode == "done":
            duration_secs = int(sys.argv[2])
            ext = sys.argv[3]
            default_path = sys.argv[4]
            build_done(application, duration_secs, ext, default_path)
        elif mode == "bar":
            elapsed_secs = int(sys.argv[2])
            paused = sys.argv[3] == "1"
            busy = sys.argv[4] == "1"
            build_bar(application, elapsed_secs, paused, busy)
        elif mode == "toast":
            build_toast(application)
        elif mode == "delay":
            remaining_secs = int(sys.argv[2])
            build_delay(application, remaining_secs)
        elif mode == "error":
            build_error(application, sys.argv[2])
        elif mode == "history":
            build_history(application)
        elif mode == "editor":
            base_png = sys.argv[2]
            w = int(sys.argv[3])
            h = int(sys.argv[4])
            dark = sys.argv[5] == "1"
            build_editor(application, base_png, w, h, dark)

    app.connect("activate", on_activate)
    app.run(None)


if __name__ == "__main__":
    main()

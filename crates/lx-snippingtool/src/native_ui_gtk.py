import io
import json
import math
import os
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
.integrated-dock {
    background-color: alpha(currentColor, 0.035);
    padding: 10px 8px;
}
.canvas-well {
    background-color: alpha(#000000, 0.16);
}
.dock-section-title {
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.6px;
    opacity: 0.65;
}
.dock-tool-btn {
    min-width: 36px;
    min-height: 32px;
    padding: 4px 6px;
    border-radius: 7px;
}
.dock-prop-btn {
    min-width: 62px;
    min-height: 26px;
    padding: 3px 6px;
    border-radius: 7px;
    font-size: 11.5px;
}
.status-bar-strip {
    background-color: alpha(currentColor, 0.035);
    padding: 5px 14px;
}
"""

TOOL_SVG_TEMPLATES = {
    "pen": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M11.5 2.5l2 2L5.5 12.5l-3 .7.7-3 8.3-7.7z"/></svg>',
    "highlighter": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M10.5 2.5l3 3-6.5 6.5-3.5.5.5-3.5 6.5-6.5zM2.5 14h11"/></svg>',
    "spray": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.35" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="6.5" width="6" height="7" rx="1"/><path d="M4.8 6.5V4.5h2.4v2M6 3v1.5M9 5l1.5-.8M10.5 6.5h1.5M9 8l1.5.8"/><circle cx="13" cy="3.5" r="0.7" fill="{color}" stroke="none"/><circle cx="13.8" cy="6.5" r="0.7" fill="{color}" stroke="none"/><circle cx="13" cy="9.5" r="0.7" fill="{color}" stroke="none"/></svg>',
    "eraser": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M7 13.5h6.5M3.2 9.5l5.3-5.3a1.5 1.5 0 012.1 0l2.2 2.2a1.5 1.5 0 010 2.1L8.5 12.8a1.5 1.5 0 01-2.1 0L3.2 9.6a1.5 1.5 0 010-2.1z"/></svg>',
    "style_solid": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.8" stroke-linecap="round"><path d="M2.5 8h11"/></svg>',
    "style_dotted": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="{color}"><circle cx="3" cy="8" r="1.15"/><circle cx="6.3" cy="8" r="1.15"/><circle cx="9.7" cy="8" r="1.15"/><circle cx="13" cy="8" r="1.15"/></svg>',
    "style_dashed": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.8" stroke-linecap="round"><path d="M2 8h3M6.5 8h3M11 8h3"/></svg>',
    "line": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.6" stroke-linecap="round"><path d="M3 13L13 3"/></svg>',
    "curve": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.5" stroke-linecap="round"><path d="M2.5 12.5C2.5 6.5 13.5 9.5 13.5 3.5"/><circle cx="2.5" cy="12.5" r="1.2" fill="{color}" stroke="none"/><circle cx="13.5" cy="3.5" r="1.2" fill="{color}" stroke="none"/></svg>',
    "rect": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4"><rect x="2.5" y="3.5" width="11" height="9" rx="1.2"/></svg>',
    "ellipse": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4"><circle cx="8" cy="8" r="5.2"/><ellipse cx="8" cy="8" rx="5.2" ry="2.1" stroke-opacity="0.55"/></svg>',
    "arrow": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 13L13 3M7.5 3H13v5.5"/></svg>',
    "double_arrow": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.45" stroke-linecap="round" stroke-linejoin="round"><path d="M3 13L13 3M9 3h4v4M7 13H3V9"/></svg>',
    "redact": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="{color}"><rect x="2" y="3.5" width="12" height="9" rx="1.8" fill-opacity="0.28"/><rect x="3.2" y="4.7" width="9.6" height="6.6" rx="1.1"/></svg>',
    "crop": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round"><path d="M4.5 2v9.5H14M2 4.5h9.5V14"/></svg>',
    "new": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.6" stroke-linecap="round"><path d="M8 3v10M3 8h10"/></svg>',
    "save": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linejoin="round"><path d="M3 2.5h8l2.5 2.5v8.5h-11v-11z"/><path d="M5.5 2.5v3.5h4.5V2.5M5 13.5v-4h6v4"/></svg>',
    "copy": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linejoin="round"><rect x="5.5" y="5.5" width="8" height="8" rx="1.2"/><path d="M10.5 5.5V3.5a1 1 0 00-1-1h-6a1 1 0 00-1 1v6a1 1 0 001 1h2"/></svg>',
    "history": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round"><path d="M2.8 8a5.2 5.2 0 101.2-3.4M2.5 2.8v2.4h2.4M8 5.5V8l2 1.2"/></svg>',
    "undo": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M5.5 4L2.5 7l3 3M3 7h6.5a3.5 3.5 0 110 7H7.5"/></svg>',
    "redo": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M10.5 4l3 3-3 3M13 7H6.5a3.5 3.5 0 100 7H8.5"/></svg>',
    "theme_sun": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round"><circle cx="8" cy="8" r="3"/><path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1"/></svg>',
    "theme_moon": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M12.8 10.2A5.2 5.2 0 015.8 3.2a5.2 5.2 0 107 7z"/></svg>',
    "palette_add": '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" stroke-width="1.5" stroke-linecap="round"><circle cx="8" cy="8" r="5.5" stroke-dasharray="2.2 1.8"/><path d="M8 5.5v5M5.5 8h5"/></svg>',
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


def build_editor(app, base_png_bytes: bytes, img_w: int, img_h: int, initial_dark: bool):
    install_css()
    style_mgr = Adw.StyleManager.get_default()
    style_mgr.set_color_scheme(
        Adw.ColorScheme.FORCE_DARK if initial_dark else Adw.ColorScheme.FORCE_LIGHT
    )

    loaded_surface = cairo.ImageSurface.create_from_png(io.BytesIO(base_png_bytes))
    if loaded_surface.get_format() in (cairo.FORMAT_ARGB32, cairo.FORMAT_RGB24):
        base_surface = loaded_surface
    else:
        base_surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, img_w, img_h)
        _conv_cr = cairo.Context(base_surface)
        _conv_cr.set_source_surface(loaded_surface, 0.0, 0.0)
        _conv_cr.paint()
    base_surface.flush()
    base_data = base_surface.get_data()
    base_stride = base_surface.get_stride()

    state = {
        "closed_by_host": False,
        "emitted_close": False,
        "dark": initial_dark,
        "tool": "pen",
        "line_style": "Solid",
        "color": {"r": 220, "g": 38, "b": 38, "a": 255},
        "custom_colors": [],
        "width": 4.0,
        "ops": [],
        "crop_rect": None,
        "dragging": False,
        "drag_start": None,
        "drag_current": None,
        "points": [],
        "curve_waypoints": [],
        "hover_pt": None,
        "curve_drag_mode": False,
        "redact_cache": {},
    }

    win = Adw.ApplicationWindow(
        application=app,
        title="LX Snipping Tool",
        default_width=1100,
        default_height=720,
    )

    toolbar_view = Adw.ToolbarView()
    header = Adw.HeaderBar()
    toolbar_view.add_top_bar(header)

    svg_images = []

    def current_icon_hex():
        return "#f4f4f6" if state["dark"] else "#1a1a1c"

    def make_icon_text_btn(icon_key: str, text_str: str, fixed_hex=None):
        box = Gtk.Box(
            orientation=Gtk.Orientation.HORIZONTAL,
            spacing=6,
            halign=Gtk.Align.CENTER,
            valign=Gtk.Align.CENTER,
        )
        hex_col = fixed_hex if fixed_hex else current_icon_hex()
        img = Gtk.Image.new_from_paintable(make_svg_texture(icon_key, hex_col))
        img.set_pixel_size(15)
        if fixed_hex is None:
            svg_images.append((icon_key, img))
        lbl = Gtk.Label(label=text_str)
        box.append(img)
        box.append(lbl)
        return Gtk.Button(child=box), img, lbl

    def make_icon_only_btn(icon_key: str, tip: str):
        img = Gtk.Image.new_from_paintable(make_svg_texture(icon_key, current_icon_hex()))
        img.set_pixel_size(15)
        svg_images.append((icon_key, img))
        btn = Gtk.Button(child=img, tooltip_text=tip)
        return btn, img

    def commit_curve_waypoints():
        pts = state["curve_waypoints"]
        if not pts:
            return
        state["curve_waypoints"] = []
        state["curve_drag_mode"] = False
        if len(pts) >= 2:
            emit_json(
                {
                    "cmd": "push_op",
                    "op": {
                        "CurvedLine": {
                            "points": list(pts),
                            "color": state["color"],
                            "width": state["width"],
                            "style": state["line_style"],
                        }
                    },
                }
            )
        area.queue_draw()

    def trigger_host_cmd(cmd_dict):
        commit_curve_waypoints()
        emit_json(cmd_dict)

    # Option D Unified HeaderBar: Left (New | History | Undo/Redo) — Center Title — Right (Theme | Save | Copy)
    new_btn, _, _ = make_icon_text_btn("new", "New")
    new_btn.set_tooltip_text("Nova captura")
    new_btn.connect("clicked", lambda _b: trigger_host_cmd({"cmd": "new"}))

    history_btn, _, _ = make_icon_text_btn("history", "History")
    history_btn.set_tooltip_text("Histórico de capturas")
    history_btn.connect("clicked", lambda _b: trigger_host_cmd({"cmd": "history"}))

    undo_btn, _ = make_icon_only_btn("undo", "Desfazer (Ctrl+Z)")
    undo_btn.set_sensitive(False)
    undo_btn.connect("clicked", lambda _b: trigger_host_cmd({"cmd": "undo"}))

    redo_btn, _ = make_icon_only_btn("redo", "Refazer (Ctrl+Y)")
    redo_btn.set_sensitive(False)
    redo_btn.connect("clicked", lambda _b: trigger_host_cmd({"cmd": "redo"}))

    undo_redo_group = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=0)
    undo_redo_group.add_css_class("linked")
    undo_redo_group.append(undo_btn)
    undo_redo_group.append(redo_btn)

    reset_crop_btn = Gtk.Button(label="Reset Crop", visible=False)
    reset_crop_btn.set_tooltip_text("Limpar área de recorte")
    reset_crop_btn.connect("clicked", lambda _b: trigger_host_cmd({"cmd": "set_crop", "crop": None}))

    header.pack_start(new_btn)
    header.pack_start(history_btn)
    header.pack_start(undo_redo_group)
    header.pack_start(reset_crop_btn)

    copy_btn, _, _ = make_icon_text_btn("copy", "Copy", fixed_hex="#ffffff")
    copy_btn.add_css_class("suggested-action")
    copy_btn.set_tooltip_text("Copiar para a área de transferência (Ctrl+C)")
    copy_btn.connect("clicked", lambda _b: trigger_host_cmd({"cmd": "copy"}))

    save_btn, _, _ = make_icon_text_btn("save", "Save")
    save_btn.set_tooltip_text("Salvar imagem (Ctrl+S)")
    save_btn.connect("clicked", lambda _b: trigger_host_cmd({"cmd": "save"}))

    theme_icon_key = "theme_sun" if initial_dark else "theme_moon"
    theme_btn, theme_img = make_icon_only_btn(
        theme_icon_key, "Alternar tema Claro / Escuro"
    )

    # pack_end adds from outermost right inward: Copy rightmost, then Save, then Theme
    header.pack_end(copy_btn)
    header.pack_end(save_btn)
    header.pack_end(theme_btn)

    # Main vertical container below HeaderBar: [Horizontal Workspace (Left Dock | Canvas | Right Dock)] + [Status Bar]
    root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=0)

    body_row = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
        spacing=0,
        hexpand=True,
        vexpand=True,
    )

    # 1. Integrated Left Tool Dock (Non-floating, flush with window edge & HeaderBar)
    left_dock = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=5,
        valign=Gtk.Align.FILL,
        vexpand=True,
    )
    left_dock.add_css_class("integrated-dock")

    tool_defs = [
        ("pen", "Caneta (Traço Livre)"),
        ("highlighter", "Marca-texto"),
        ("spray", "Spray / Aerógrafo"),
        ("eraser", "Borracha"),
        None,
        ("rect", "Retângulo"),
        ("ellipse", "Esfera / Elipse"),
        ("line", "Linha Reta"),
        ("curve", "Linha Curva (Spline Catmull-Rom / Bézier)"),
        ("arrow", "Seta"),
        ("double_arrow", "Seta com Ponta Dupla"),
        None,
        ("redact", "Tarja Vidro Fosco (Escuro 50% + Dispersão)"),
        ("crop", "Recortar (Crop)"),
    ]

    tool_label_map = {
        "pen": "Caneta",
        "highlighter": "Marca-texto",
        "spray": "Spray",
        "eraser": "Borracha",
        "rect": "Retângulo",
        "ellipse": "Esfera",
        "line": "Linha",
        "curve": "Linha Curva",
        "arrow": "Seta",
        "double_arrow": "Seta Dupla",
        "redact": "Vidro Fosco",
        "crop": "Crop",
    }

    first_tool_btn = None

    def refresh_all_icons():
        hex_color = current_icon_hex()
        for key, img in svg_images:
            actual_key = (
                ("theme_sun" if state["dark"] else "theme_moon")
                if key in ("theme_sun", "theme_moon")
                else key
            )
            img.set_from_paintable(make_svg_texture(actual_key, hex_color))

    def format_status_text(custom_msg=None):
        base = custom_msg if custom_msg else "Ready"
        tname = tool_label_map.get(state["tool"], state["tool"])
        sname = {
            "Solid": "Sólida",
            "Dotted": "Pontilhada",
            "Dashed": "Tracejada",
        }.get(state["line_style"], state["line_style"])
        w_int = int(round(state["width"]))
        w_tag = "S" if w_int <= 2 else ("M" if w_int <= 4 else "L")
        if state["tool"] == "curve":
            return f"{base}  •  {tname} ({sname} • {w_tag} {w_int}px) — Clique nos pontos (duplo clique, botão direito ou Enter finaliza) ou arraste"
        return f"{base}  •  Ferramenta: {tname} ({sname} • {w_tag} {w_int}px)"

    def update_status_banner(custom_msg=None):
        status_label.set_text(format_status_text(custom_msg))

    def on_tool_toggled(btn, key):
        if btn.get_active():
            commit_curve_waypoints()
            state["tool"] = key
            update_status_banner()

    for entry in tool_defs:
        if entry is None:
            sep = Gtk.Separator(orientation=Gtk.Orientation.HORIZONTAL)
            sep.set_margin_top(4)
            sep.set_margin_bottom(4)
            left_dock.append(sep)
            continue
        key, tip = entry
        tex = make_svg_texture(key, current_icon_hex())
        img = Gtk.Image.new_from_paintable(tex)
        img.set_pixel_size(16)
        svg_images.append((key, img))
        btn = Gtk.ToggleButton(child=img, tooltip_text=tip, halign=Gtk.Align.CENTER)
        btn.add_css_class("flat")
        btn.add_css_class("dock-tool-btn")
        if first_tool_btn is None:
            first_tool_btn = btn
            btn.set_active(True)
        else:
            btn.set_group(first_tool_btn)
        btn.connect("toggled", on_tool_toggled, key)
        left_dock.append(btn)

    # 2. Central Recessed Canvas Well
    area = Gtk.DrawingArea(hexpand=True, vexpand=True)
    area.add_css_class("canvas-well")

    # 3. Integrated Right Inspector Dock (LINHA, ESPESSURA, CORES)
    right_dock = Gtk.Box(
        orientation=Gtk.Orientation.VERTICAL,
        spacing=5,
        valign=Gtk.Align.FILL,
        vexpand=True,
    )
    right_dock.add_css_class("integrated-dock")

    lbl_line = Gtk.Label(label="LINHA", halign=Gtk.Align.CENTER, margin_top=2, margin_bottom=2)
    lbl_line.add_css_class("dock-section-title")
    right_dock.append(lbl_line)

    style_defs = [
        ("style_solid", "Solid", "Linha Sólida"),
        ("style_dotted", "Dotted", "Linha Pontilhada"),
        ("style_dashed", "Dashed", "Linha Tracejada"),
    ]
    first_style_btn = None

    def on_style_toggled(btn, style_name):
        if btn.get_active():
            state["line_style"] = style_name
            update_status_banner()

    for skey, sname, stip in style_defs:
        tex = make_svg_texture(skey, current_icon_hex())
        img = Gtk.Image.new_from_paintable(tex)
        img.set_pixel_size(16)
        svg_images.append((skey, img))
        sbtn = Gtk.ToggleButton(child=img, tooltip_text=stip, halign=Gtk.Align.CENTER)
        sbtn.add_css_class("flat")
        sbtn.add_css_class("dock-prop-btn")
        if first_style_btn is None:
            first_style_btn = sbtn
            sbtn.set_active(True)
        else:
            sbtn.set_group(first_style_btn)
        sbtn.connect("toggled", on_style_toggled, sname)
        right_dock.append(sbtn)

    sep_r1 = Gtk.Separator(orientation=Gtk.Orientation.HORIZONTAL)
    sep_r1.set_margin_top(6)
    sep_r1.set_margin_bottom(6)
    right_dock.append(sep_r1)

    lbl_width = Gtk.Label(label="ESPESSURA", halign=Gtk.Align.CENTER, margin_bottom=2)
    lbl_width.add_css_class("dock-section-title")
    right_dock.append(lbl_width)

    width_defs = [("S   2px", 2.0), ("M   4px", 4.0), ("L   8px", 8.0)]
    first_width_btn = None

    def on_width_toggled(btn, w_val):
        if btn.get_active():
            state["width"] = w_val
            update_status_banner()

    for label, w_val in width_defs:
        wbtn = Gtk.ToggleButton(label=label, halign=Gtk.Align.CENTER)
        wbtn.add_css_class("flat")
        wbtn.add_css_class("dock-prop-btn")
        if first_width_btn is None:
            first_width_btn = wbtn
        else:
            wbtn.set_group(first_width_btn)
        if abs(w_val - 4.0) < 0.1:
            wbtn.set_active(True)
        wbtn.connect("toggled", on_width_toggled, w_val)
        right_dock.append(wbtn)

    sep_r2 = Gtk.Separator(orientation=Gtk.Orientation.HORIZONTAL)
    sep_r2.set_margin_top(6)
    sep_r2.set_margin_bottom(6)
    right_dock.append(sep_r2)

    lbl_colors = Gtk.Label(label="CORES", halign=Gtk.Align.CENTER, margin_bottom=2)
    lbl_colors.add_css_class("dock-section-title")
    right_dock.append(lbl_colors)

    swatch_grid = Gtk.Grid(
        column_spacing=6,
        row_spacing=6,
        halign=Gtk.Align.CENTER,
        valign=Gtk.Align.START,
    )
    right_dock.append(swatch_grid)

    preset_colors = [
        {"r": 0, "g": 0, "b": 0, "a": 255},
        {"r": 220, "g": 38, "b": 38, "a": 255},
        {"r": 250, "g": 204, "b": 21, "a": 255},
        {"r": 37, "g": 99, "b": 235, "a": 255},
        {"r": 34, "g": 197, "b": 94, "a": 255},
        {"r": 255, "g": 255, "b": 255, "a": 255},
    ]

    def colors_equal(a: dict, b: dict) -> bool:
        return a["r"] == b["r"] and a["g"] == b["g"] and a["b"] == b["b"]

    def make_color_dot(col: dict):
        area_dot = Gtk.DrawingArea(width_request=16, height_request=16)

        def draw_dot(_da, cr, w, h):
            r = min(w, h) * 0.5 - 0.5
            cr.arc(w * 0.5, h * 0.5, r, 0.0, math.tau)
            cr.set_source_rgb(col["r"] / 255.0, col["g"] / 255.0, col["b"] / 255.0)
            cr.fill_preserve()
            cr.set_source_rgba(0.5, 0.5, 0.55, 0.45)
            cr.set_line_width(1.0)
            cr.stroke()

        area_dot.set_draw_func(draw_dot)
        return area_dot

    def open_custom_color_dialog(slot_idx=None):
        initial = (
            state["custom_colors"][slot_idx]
            if (slot_idx is not None and slot_idx < len(state["custom_colors"]))
            else state["color"]
        )
        rgba = Gdk.RGBA()
        rgba.red = initial["r"] / 255.0
        rgba.green = initial["g"] / 255.0
        rgba.blue = initial["b"] / 255.0
        rgba.alpha = 1.0

        def apply_picked(c_rgba):
            picked = {
                "r": int(round(c_rgba.red * 255.0)),
                "g": int(round(c_rgba.green * 255.0)),
                "b": int(round(c_rgba.blue * 255.0)),
                "a": 255,
            }
            if slot_idx is not None and slot_idx < len(state["custom_colors"]):
                state["custom_colors"][slot_idx] = picked
            elif len(state["custom_colors"]) < 3:
                state["custom_colors"].append(picked)
            state["color"] = dict(picked)
            rebuild_swatches()

        if hasattr(Gtk, "ColorDialog"):
            dlg = Gtk.ColorDialog(with_alpha=False)

            def on_finish(d, res):
                try:
                    c = d.choose_rgba_finish(res)
                    apply_picked(c)
                except Exception:
                    pass

            dlg.choose_rgba(win, rgba, None, on_finish)
        else:
            dlg = Gtk.ColorChooserDialog(
                title="Escolher cor personalizada", transient_for=win, modal=True
            )
            dlg.set_use_alpha(False)
            dlg.set_rgba(rgba)

            def on_resp(d, resp):
                if resp == Gtk.ResponseType.OK:
                    apply_picked(d.get_rgba())
                d.destroy()

            dlg.connect("response", on_resp)
            dlg.present()

    def rebuild_swatches():
        child = swatch_grid.get_first_child()
        while child is not None:
            nxt = child.get_next_sibling()
            swatch_grid.remove(child)
            child = nxt

        slot_pos = 0
        for col in preset_colors:
            dot = make_color_dot(col)
            sbtn = Gtk.Button(child=dot)
            sbtn.add_css_class("flat")
            sbtn.add_css_class("swatch-btn")
            if colors_equal(state["color"], col):
                sbtn.add_css_class("swatch-active")
            sbtn.connect("clicked", lambda _b, c=col: set_active_color(c))
            swatch_grid.attach(sbtn, slot_pos % 2, slot_pos // 2, 1, 1)
            slot_pos += 1

        for idx, col in enumerate(state["custom_colors"]):
            dot = make_color_dot(col)
            sbtn = Gtk.Button(
                child=dot,
                tooltip_text=f"Cor personalizada #{idx + 1} (clique direito para alterar)",
            )
            sbtn.add_css_class("flat")
            sbtn.add_css_class("swatch-btn")
            if colors_equal(state["color"], col):
                sbtn.add_css_class("swatch-active")
            sbtn.connect("clicked", lambda _b, c=col: set_active_color(c))
            right_click = Gtk.GestureClick(button=3)
            right_click.connect("pressed", lambda _g, _n, _x, _y, i=idx: open_custom_color_dialog(i))
            sbtn.add_controller(right_click)
            swatch_grid.attach(sbtn, slot_pos % 2, slot_pos // 2, 1, 1)
            slot_pos += 1

        if len(state["custom_colors"]) < 3:
            add_img = Gtk.Image.new_from_paintable(
                make_svg_texture("palette_add", current_icon_hex())
            )
            add_img.set_pixel_size(16)
            svg_images.append(("palette_add", add_img))
            add_btn = Gtk.Button(
                child=add_img,
                tooltip_text=f"Adicionar cor personalizada ({len(state['custom_colors'])}/3)",
            )
            add_btn.add_css_class("flat")
            add_btn.add_css_class("swatch-btn")
            add_btn.connect("clicked", lambda _b: open_custom_color_dialog(None))
            swatch_grid.attach(add_btn, slot_pos % 2, slot_pos // 2, 1, 1)

    def set_active_color(col: dict):
        state["color"] = dict(col)
        rebuild_swatches()

    rebuild_swatches()

    def on_toggle_theme(_btn):
        state["dark"] = not state["dark"]
        style_mgr.set_color_scheme(
            Adw.ColorScheme.FORCE_DARK if state["dark"] else Adw.ColorScheme.FORCE_LIGHT
        )
        refresh_all_icons()
        rebuild_swatches()
        area.queue_draw()

    theme_btn.connect("clicked", on_toggle_theme)

    # Assemble horizontal 3-pane workspace with 1px structural borders
    body_row.append(left_dock)
    body_row.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
    body_row.append(area)
    body_row.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
    body_row.append(right_dock)

    root.append(Gtk.Separator(orientation=Gtk.Orientation.HORIZONTAL))
    root.append(body_row)
    root.append(Gtk.Separator(orientation=Gtk.Orientation.HORIZONTAL))

    # Bottom status bar flush across the window
    status_row = Gtk.Box(
        orientation=Gtk.Orientation.HORIZONTAL,
    )
    status_row.add_css_class("status-bar-strip")
    status_label = Gtk.Label(
        label=format_status_text(),
        xalign=0.0,
        hexpand=True,
        halign=Gtk.Align.START,
    )
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

    def resolve_rect_bounds(r):
        if r is None:
            return (0, 0, img_w, img_h, False)
        rx = min(float(r["x"]), float(r["x"]) + float(r["width"]))
        ry = min(float(r["y"]), float(r["y"]) + float(r["height"]))
        rw = abs(float(r["width"]))
        rh = abs(float(r["height"]))
        cx = max(0.0, min(float(img_w), rx))
        cy = max(0.0, min(float(img_h), ry))
        cx1 = max(0.0, min(float(img_w), rx + rw))
        cy1 = max(0.0, min(float(img_h), ry + rh))
        x0 = min(max(int(math.floor(cx)), 0), img_w)
        y0 = min(max(int(math.floor(cy)), 0), img_h)
        x1 = min(max(int(math.ceil(cx1)), 0), img_w)
        y1 = min(max(int(math.ceil(cy1)), 0), img_h)
        vw = x1 - x0
        vh = y1 - y0
        if vw > 0 and vh > 0:
            return (x0, y0, vw, vh, True)
        return (0, 0, img_w, img_h, False)

    def visible_crop_bounds():
        return resolve_rect_bounds(state["crop_rect"])

    def update_dim_label():
        _, _, vw, vh, is_cropped = visible_crop_bounds()
        if is_cropped:
            dim_label.set_label(f"{vw} × {vh} px (recortado)")
        else:
            dim_label.set_label(f"{img_w} × {img_h} px")

    def fit_rect(w: float, h: float):
        vx, vy, vw, vh, _ = visible_crop_bounds()
        pad = 24.0
        avail_w = max(w - pad, 1.0)
        avail_h = max(h - pad, 1.0)
        scale = min(avail_w / max(vw, 1), avail_h / max(vh, 1), 1.0)
        draw_w = vw * scale
        draw_h = vh * scale
        frame_x = (w - draw_w) * 0.5
        frame_y = (h - draw_h) * 0.5
        return (
            frame_x,
            frame_y,
            draw_w,
            draw_h,
            scale,
            float(vx),
            float(vy),
            float(vw),
            float(vh),
        )

    def to_image_point(w: float, h: float, cx: float, cy: float):
        fx, fy, fw, fh, _, vx, vy, vw, vh = fit_rect(w, h)
        ix = max(vx, min(vx + vw, vx + ((cx - fx) / max(fw, 1.0)) * vw))
        iy = max(vy, min(vy + vh, vy + ((cy - fy) / max(fh, 1.0)) * vh))
        return {"x": ix, "y": iy}

    def set_rgba(cr, col: dict, alpha_mul: float = 1.0):
        a = max(0.0, min(1.0, (col["a"] / 255.0) * alpha_mul))
        cr.set_source_rgba(col["r"] / 255.0, col["g"] / 255.0, col["b"] / 255.0, a)

    def apply_line_style(cr, style: str, sw: float):
        if style == "Dotted":
            cr.set_dash([0.1, max(sw * 2.2, 2.0)], 0.0)
        elif style == "Dashed":
            cr.set_dash([max(sw * 3.0, 3.0), max(sw * 2.0, 2.0)], 0.0)
        else:
            cr.set_dash([], 0.0)

    def spray_stamp_points(pts, spacing: float):
        if not pts:
            return []
        if len(pts) == 1:
            return [(pts[0]["x"], pts[0]["y"])]
        step = max(spacing, 1.0)
        out = [(pts[0]["x"], pts[0]["y"])]
        residual = 0.0
        for i in range(len(pts) - 1):
            ax, ay = pts[i]["x"], pts[i]["y"]
            bx, by = pts[i + 1]["x"], pts[i + 1]["y"]
            dx, dy = bx - ax, by - ay
            seg_len = math.hypot(dx, dy)
            if seg_len <= 1e-4:
                continue
            d = step - residual
            while d <= seg_len:
                t = max(0.0, min(1.0, d / seg_len))
                out.append((ax + dx * t, ay + dy * t))
                d += step
            residual = seg_len - (d - step)
        return out

    def draw_spray(cr, fx, fy, scale, pts, col, radius):
        if not pts:
            return
        soft_r = max(radius * 1.35, 5.0)
        spacing = max(1.5, min(6.0, soft_r * 0.25))
        stamps = spray_stamp_points(pts, spacing)
        boost = 2.2 if len(stamps) == 1 else 1.0
        r_px = soft_r * scale
        cr_r = col["r"] / 255.0
        cr_g = col["g"] / 255.0
        cr_b = col["b"] / 255.0
        base_a = (col["a"] / 255.0) * boost
        for sx, sy in stamps:
            cx = fx + sx * scale
            cy = fy + sy * scale
            grad = cairo.RadialGradient(cx, cy, 0.0, cx, cy, r_px)
            grad.add_color_stop_rgba(0.00, cr_r, cr_g, cr_b, min(1.0, base_a * 0.22))
            grad.add_color_stop_rgba(0.30, cr_r, cr_g, cr_b, min(1.0, base_a * 0.14))
            grad.add_color_stop_rgba(0.62, cr_r, cr_g, cr_b, min(1.0, base_a * 0.045))
            grad.add_color_stop_rgba(0.85, cr_r, cr_g, cr_b, min(1.0, base_a * 0.010))
            grad.add_color_stop_rgba(1.00, cr_r, cr_g, cr_b, 0.0)
            cr.set_source(grad)
            cr.arc(cx, cy, r_px, 0.0, math.tau)
            cr.fill()

    def frosted_hash01(x: int, y: int, seed: int = 0x9E3779B9) -> float:
        h = (((x & 0xFFFFFFFF) * 0x9E3779B1) ^ ((y & 0xFFFFFFFF) * 0x85EBCA77) ^ ((seed & 0xFFFFFFFF) * 0xC2B2AE3D)) & 0xFFFFFFFF
        h = (h ^ (h >> 15)) & 0xFFFFFFFF
        h = (h * 0x2C1B3C6D) & 0xFFFFFFFF
        h = (h ^ (h >> 12)) & 0xFFFFFFFF
        h = (h * 0x297A2D39) & 0xFFFFFFFF
        h = (h ^ (h >> 15)) & 0xFFFFFFFF
        return h / 4294967296.0

    def build_smoked_frosted_grid(ix0: int, iy0: int, ix1: int, iy1: int):
        box_w = max(1, ix1 - ix0)
        box_h = max(1, iy1 - iy0)
        cell = max(4, min(10, int(round(min(box_w, box_h) * 0.25))))
        gw = max(1, (box_w + cell - 1) // cell)
        gh = max(1, (box_h + cell - 1) // cell)
        is_le = sys.byteorder == "little"
        grid = [(30, 32, 36)] * (gw * gh)
        for gy in range(gh):
            ys = max(0, min(img_h - 1, iy0 + gy * cell))
            ye = max(ys + 1, min(img_h, iy0 + (gy + 1) * cell))
            for gx in range(gw):
                xs = max(0, min(img_w - 1, ix0 + gx * cell))
                xe = max(xs + 1, min(img_w, ix0 + (gx + 1) * cell))
                sr = sg = sb = cnt = 0
                for sy in range(ys, ye, 2):
                    row = sy * base_stride
                    for sx in range(xs, xe, 2):
                        off = row + sx * 4
                        if is_le:
                            b, g, r = base_data[off], base_data[off + 1], base_data[off + 2]
                        else:
                            r, g, b = base_data[off + 1], base_data[off + 2], base_data[off + 3]
                        sr += r
                        sg += g
                        sb += b
                        cnt += 1
                if cnt:
                    grid[gy * gw + gx] = (
                        int((sr / cnt) * 0.5 + 12.0 * 0.5),
                        int((sg / cnt) * 0.5 + 14.0 * 0.5),
                        int((sb / cnt) * 0.5 + 18.0 * 0.5),
                    )
        smooth = list(grid)
        for gy in range(gh):
            for gx in range(gw):
                sr = sg = sb = cnt = 0
                for dy in (-1, 0, 1):
                    ny = gy + dy
                    if 0 <= ny < gh:
                        for dx in (-1, 0, 1):
                            nx = gx + dx
                            if 0 <= nx < gw:
                                r, g, b = grid[ny * gw + nx]
                                sr += r
                                sg += g
                                sb += b
                                cnt += 1
                smooth[gy * gw + gx] = (sr // cnt, sg // cnt, sb // cnt)
        return smooth, gw, gh, cell

    def draw_redact_box(cr, fx, fy, scale, rx_img, ry_img, rw_img, rh_img, is_preview=False):
        if rw_img <= 0.0 or rh_img <= 0.0:
            return
        ix0 = max(0, min(img_w - 1, math.floor(rx_img)))
        iy0 = max(0, min(img_h - 1, math.floor(ry_img)))
        ix1 = max(ix0 + 1, min(img_w, math.ceil(rx_img + rw_img)))
        iy1 = max(iy0 + 1, min(img_h, math.ceil(ry_img + rh_img)))
        box_w = max(1, ix1 - ix0)
        box_h = max(1, iy1 - iy0)
        scatter_r = max(2.5, min(16.0, min(box_w, box_h) * 0.40))
        pad = math.ceil(scatter_r)

        if is_preview:
            # Prévia nítida durante o arraste: preenchimento fumê suave + borda pontilhada contrastada
            rx = fx + ix0 * scale
            ry = fy + iy0 * scale
            rw = box_w * scale
            rh = box_h * scale
            cr.set_source_rgba(12.0 / 255.0, 14.0 / 255.0, 18.0 / 255.0, 0.42)
            cr.rectangle(rx, ry, rw, rh)
            cr.fill()
            cr.save()
            cr.set_line_cap(cairo.LINE_CAP_ROUND)
            cr.set_dash([0.5, 6.0], 0.0)
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.80)
            cr.set_line_width(3.0)
            cr.rectangle(rx, ry, rw, rh)
            cr.stroke()
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.96)
            cr.set_line_width(1.8)
            cr.rectangle(rx, ry, rw, rh)
            cr.stroke()
            cr.restore()
            return

        cache_key = (ix0, iy0, ix1, iy1)
        cached = state["redact_cache"].get(cache_key)
        if cached is None:
            smooth, gw, gh, cell = build_smoked_frosted_grid(ix0, iy0, ix1, iy1)
            ds = 1 if (box_w * box_h) <= 40000 else 2
            w_s = max(1, (box_w + pad * 2 + ds - 1) // ds)
            h_s = max(1, (box_h + pad * 2 + ds - 1) // ds)
            buf = bytearray(w_s * h_s * 4)
            is_le = sys.byteorder == "little"

            for ly in range(h_s):
                py = iy0 - pad + ly * ds
                cy = py + 0.5
                dy = (iy0 - cy) if cy < iy0 else ((cy - iy1) if cy > iy1 else 0.0)
                in_y = iy0 <= py < iy1
                row_base = ly * w_s * 4
                for lx in range(w_s):
                    px = ix0 - pad + lx * ds
                    h = (((px & 0xFFFFFFFF) * 374761393) ^ ((py & 0xFFFFFFFF) * 668265263)) & 0xFFFFFFFF
                    h = ((h ^ (h >> 13)) * 1274126177) & 0xFFFFFFFF
                    h = (h ^ (h >> 16)) & 0xFFFFFFFF
                    if in_y and ix0 <= px < ix1:
                        ox = ((h & 0xFF) / 127.5 - 1.0) * scatter_r
                        oy = (((h >> 8) & 0xFF) / 127.5 - 1.0) * scatter_r
                        sx = min(gw - 1, max(0, int((px - ix0 + ox) // cell)))
                        sy = min(gh - 1, max(0, int((py - iy0 + oy) // cell)))
                        r, g, b = smooth[sy * gw + sx]
                        grain = (((h >> 16) & 0x1F) - 16) // 2
                        out_r = max(0, min(255, r + grain))
                        out_g = max(0, min(255, g + grain))
                        out_b = max(0, min(255, b + grain))
                        idx = row_base + lx * 4
                        if is_le:
                            buf[idx] = out_b
                            buf[idx + 1] = out_g
                            buf[idx + 2] = out_r
                            buf[idx + 3] = 255
                        else:
                            buf[idx] = 255
                            buf[idx + 1] = out_r
                            buf[idx + 2] = out_g
                            buf[idx + 3] = out_b
                    else:
                        cx = px + 0.5
                        dx = (ix0 - cx) if cx < ix0 else ((cx - ix1) if cx > ix1 else 0.0)
                        dist = math.hypot(dx, dy)
                        t = 1.0 - dist / scatter_r
                        if t > 0.0:
                            prob = t ** 1.55
                            hv = (h & 0xFFFF) / 65536.0
                            if hv < prob:
                                sx = min(gw - 1, max(0, int((px - ix0) // cell)))
                                sy = min(gh - 1, max(0, int((py - iy0) // cell)))
                                r, g, b = smooth[sy * gw + sx]
                                a = min(1.0, 0.72 + 0.28 * math.sqrt((prob - hv) / prob))
                                ia = int(round(a * 255.0))
                                pr = int(round(r * a))
                                pg = int(round(g * a))
                                pb = int(round(b * a))
                                idx = row_base + lx * 4
                                if is_le:
                                    buf[idx] = pb
                                    buf[idx + 1] = pg
                                    buf[idx + 2] = pr
                                    buf[idx + 3] = ia
                                else:
                                    buf[idx] = ia
                                    buf[idx + 1] = pr
                                    buf[idx + 2] = pg
                                    buf[idx + 3] = pb

            surf = cairo.ImageSurface.create_for_data(buf, cairo.FORMAT_ARGB32, w_s, h_s)
            if len(state["redact_cache"]) >= 32:
                oldest_key = next(iter(state["redact_cache"]))
                del state["redact_cache"][oldest_key]
            state["redact_cache"][cache_key] = (surf, buf, ds)
        else:
            surf, _, ds = cached

        cr.save()
        cr.translate(fx + (ix0 - pad) * scale, fy + (iy0 - pad) * scale)
        cr.scale(scale * ds, scale * ds)
        cr.set_source_surface(surf, 0.0, 0.0)
        cr.get_source().set_filter(cairo.FILTER_NEAREST)
        cr.paint()
        cr.restore()

    def catmull_rom_spline(raw_pts: list):
        if len(raw_pts) <= 2:
            return list(raw_pts)

        if len(raw_pts) <= 24:
            min_dist_sq = 2.0 * 2.0
            end_snap_sq = 2.0 * 2.0
        else:
            min_dist_sq = 18.0 * 18.0
            end_snap_sq = 8.0 * 8.0

        knots = [raw_pts[0]]
        for pt in raw_pts[1:-1]:
            last = knots[-1]
            dx = pt["x"] - last["x"]
            dy = pt["y"] - last["y"]
            if dx * dx + dy * dy >= min_dist_sq:
                knots.append(pt)
        end_pt = raw_pts[-1]
        dx_end = end_pt["x"] - knots[-1]["x"]
        dy_end = end_pt["y"] - knots[-1]["y"]
        dist_end_sq = dx_end * dx_end + dy_end * dy_end
        if len(knots) > 1 and dist_end_sq < end_snap_sq:
            knots[-1] = end_pt
        elif dist_end_sq > 1e-4:
            knots.append(end_pt)

        if len(knots) == 2 and len(raw_pts) >= 3:
            mid = raw_pts[len(raw_pts) // 2]
            d0 = (mid["x"] - knots[0]["x"]) ** 2 + (mid["y"] - knots[0]["y"]) ** 2
            d1 = (mid["x"] - knots[1]["x"]) ** 2 + (mid["y"] - knots[1]["y"]) ** 2
            if d0 > 4.0 and d1 > 4.0:
                knots.insert(1, mid)

        if len(knots) <= 2:
            return knots

        subdivisions = 16
        n = len(knots)
        out = [knots[0]]
        for i in range(n - 1):
            p0 = knots[0] if i == 0 else knots[i - 1]
            p1 = knots[i]
            p2 = knots[i + 1]
            p3 = knots[i + 2] if (i + 2) < n else knots[n - 1]

            b1x = p1["x"] + (p2["x"] - p0["x"]) / 6.0
            b1y = p1["y"] + (p2["y"] - p0["y"]) / 6.0
            b2x = p2["x"] - (p3["x"] - p1["x"]) / 6.0
            b2y = p2["y"] - (p3["y"] - p1["y"]) / 6.0

            for step in range(1, subdivisions + 1):
                t = step / float(subdivisions)
                inv = 1.0 - t
                c0 = inv * inv * inv
                c1 = 3.0 * inv * inv * t
                c2 = 3.0 * inv * t * t
                c3 = t * t * t
                out.append(
                    {
                        "x": c0 * p1["x"] + c1 * b1x + c2 * b2x + c3 * p2["x"],
                        "y": c0 * p1["y"] + c1 * b1y + c2 * b2y + c3 * p2["y"],
                    }
                )
        return out

    def draw_polyline(cr, fx, fy, scale, pts, col, width, alpha_mul=1.0, style="Solid"):
        if not pts:
            return
        set_rgba(cr, col, alpha_mul)
        if len(pts) == 1:
            r = max(width * scale * 0.5, 1.0)
            cr.arc(fx + pts[0]["x"] * scale, fy + pts[0]["y"] * scale, r, 0, math.tau)
            cr.fill()
            return
        sw = max(width * scale, 1.0)
        cr.set_line_width(sw)
        cr.set_line_cap(cairo.LINE_CAP_ROUND)
        cr.set_line_join(cairo.LINE_JOIN_ROUND)
        apply_line_style(cr, style, sw)
        cr.move_to(fx + pts[0]["x"] * scale, fy + pts[0]["y"] * scale)
        for p in pts[1:]:
            cr.line_to(fx + p["x"] * scale, fy + p["y"] * scale)
        cr.stroke()
        cr.set_dash([], 0.0)

    def draw_arrow_head(cr, tip_x, tip_y, ang, head_len):
        spread = 0.45
        cr.move_to(tip_x, tip_y)
        cr.line_to(
            tip_x - head_len * math.cos(ang - spread),
            tip_y - head_len * math.sin(ang - spread),
        )
        cr.stroke()
        cr.move_to(tip_x, tip_y)
        cr.line_to(
            tip_x - head_len * math.cos(ang + spread),
            tip_y - head_len * math.sin(ang + spread),
        )
        cr.stroke()

    def draw_op(cr, fx, fy, scale, op: dict):
        kind_key, data = next(iter(op.items()))
        if kind_key == "Pen":
            draw_polyline(
                cr,
                fx,
                fy,
                scale,
                data["points"],
                data["color"],
                data["width"],
                1.0,
                data.get("style", "Solid"),
            )
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
                "Solid",
            )
        elif kind_key == "Spray":
            draw_spray(cr, fx, fy, scale, data["points"], data["color"], data["radius"])
        elif kind_key == "CurvedLine":
            spline_pts = catmull_rom_spline(data["points"])
            draw_polyline(
                cr,
                fx,
                fy,
                scale,
                spline_pts,
                data["color"],
                data["width"],
                1.0,
                data.get("style", "Solid"),
            )
        elif kind_key == "Shape":
            ax = fx + data["start"]["x"] * scale
            ay = fy + data["start"]["y"] * scale
            bx = fx + data["end"]["x"] * scale
            by = fy + data["end"]["y"] * scale
            set_rgba(cr, data["color"], 1.0)
            sw = max(data["width"] * scale, 1.0)
            cr.set_line_width(sw)
            cr.set_line_cap(cairo.LINE_CAP_ROUND)
            cr.set_line_join(cairo.LINE_JOIN_ROUND)
            apply_line_style(cr, data.get("style", "Solid"), sw)
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
            elif skind in ("Arrow", "DoubleArrow"):
                cr.move_to(ax, ay)
                cr.line_to(bx, by)
                cr.stroke()
                cr.set_dash([], 0.0)
                dx = bx - ax
                dy = by - ay
                length = math.hypot(dx, dy)
                if length > 1e-3:
                    head_len = min(length * 0.48, max(8.0, min(28.0, data["width"] * scale * 3.5)))
                    ang = math.atan2(dy, dx)
                    draw_arrow_head(cr, bx, by, ang, head_len)
                    if skind == "DoubleArrow":
                        draw_arrow_head(cr, ax, ay, ang + math.pi, head_len)
            cr.set_dash([], 0.0)
        elif kind_key == "RedactBox":
            r = data["rect"]
            rx = min(r["x"], r["x"] + r["width"])
            ry = min(r["y"], r["y"] + r["height"])
            rw = abs(r["width"])
            rh = abs(r["height"])
            draw_redact_box(cr, fx, fy, scale, rx, ry, rw, rh)

    def on_draw(_area, cr, w, h):
        frame_x, frame_y, fw, fh, scale, vx, vy, _vw, _vh = fit_rect(float(w), float(h))
        fx = frame_x - vx * scale
        fy = frame_y - vy * scale

        cr.save()
        cr.rectangle(frame_x, frame_y, fw, fh)
        cr.clip()

        cr.save()
        cr.translate(fx, fy)
        cr.scale(scale, scale)
        cr.set_source_surface(base_surface, 0.0, 0.0)
        cr.paint()
        cr.restore()

        for op in state["ops"]:
            draw_op(cr, fx, fy, scale, op)

        # In-progress live preview
        tool = state["tool"]
        if state["dragging"] and state["drag_start"] and state["drag_current"]:
            a = state["drag_start"]
            b = state["drag_current"]
            if tool == "pen":
                draw_polyline(
                    cr,
                    fx,
                    fy,
                    scale,
                    state["points"],
                    state["color"],
                    state["width"],
                    1.0,
                    state["line_style"],
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
                    "Solid",
                )
            elif tool == "spray":
                draw_spray(
                    cr,
                    fx,
                    fy,
                    scale,
                    state["points"],
                    state["color"],
                    max(state["width"] * 3.0, 8.0),
                )
            elif tool == "curve" and state["curve_drag_mode"]:
                spline_pts = catmull_rom_spline(state["points"])
                draw_polyline(
                    cr,
                    fx,
                    fy,
                    scale,
                    spline_pts,
                    state["color"],
                    state["width"],
                    1.0,
                    state["line_style"],
                )
            elif tool in ("line", "rect", "ellipse", "arrow", "double_arrow"):
                kind_map = {
                    "line": "Line",
                    "rect": "Rectangle",
                    "ellipse": "Ellipse",
                    "arrow": "Arrow",
                    "double_arrow": "DoubleArrow",
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
                            "style": state["line_style"],
                        }
                    },
                )
            elif tool == "redact":
                rx = min(a["x"], b["x"])
                ry = min(a["y"], b["y"])
                rw = abs(b["x"] - a["x"])
                rh = abs(b["y"] - a["y"])
                draw_redact_box(cr, fx, fy, scale, rx, ry, rw, rh, is_preview=True)
            elif tool == "crop":
                rx = fx + min(a["x"], b["x"]) * scale
                ry = fy + min(a["y"], b["y"]) * scale
                rw = abs(b["x"] - a["x"]) * scale
                rh = abs(b["y"] - a["y"]) * scale
                # Escurece a região que será descartada para destacar o novo enquadramento.
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.45)
                cr.rectangle(frame_x, frame_y, fw, max(0.0, ry - frame_y))
                cr.fill()
                cr.rectangle(
                    frame_x,
                    ry + rh,
                    fw,
                    max(0.0, (frame_y + fh) - (ry + rh)),
                )
                cr.fill()
                cr.rectangle(frame_x, ry, max(0.0, rx - frame_x), rh)
                cr.fill()
                cr.rectangle(
                    rx + rw,
                    ry,
                    max(0.0, (frame_x + fw) - (rx + rw)),
                    rh,
                )
                cr.fill()
                cr.set_source_rgba(0.0, 0.47, 0.83, 0.95)
                cr.set_line_width(2.0)
                cr.rectangle(rx, ry, rw, rh)
                cr.stroke()
                if rw > 2.0 and rh > 2.0:
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.9)
                    cr.set_line_width(1.0)
                    cr.rectangle(rx + 1.0, ry + 1.0, rw - 2.0, rh - 2.0)
                    cr.stroke()

        # Pontos de passagem ativos da curva Catmull-Rom / Bézier
        if state["curve_waypoints"]:
            wps = state["curve_waypoints"]
            if len(wps) >= 2:
                spline_pts = catmull_rom_spline(wps)
                draw_polyline(
                    cr,
                    fx,
                    fy,
                    scale,
                    spline_pts,
                    state["color"],
                    state["width"],
                    1.0,
                    state["line_style"],
                )
            if state["hover_pt"] is not None:
                preview_wps = list(wps) + [state["hover_pt"]]
                spline_hover = catmull_rom_spline(preview_wps)
                draw_polyline(
                    cr,
                    fx,
                    fy,
                    scale,
                    spline_hover,
                    state["color"],
                    state["width"],
                    0.55,
                    state["line_style"],
                )
            for wp in wps:
                cx = fx + wp["x"] * scale
                cy = fy + wp["y"] * scale
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.95)
                cr.arc(cx, cy, 4.5, 0.0, math.tau)
                cr.fill()
                set_rgba(cr, state["color"], 1.0)
                cr.arc(cx, cy, 3.0, 0.0, math.tau)
                cr.fill()

        cr.restore()

        cr.set_source_rgba(0.5, 0.5, 0.55, 0.35)
        cr.set_line_width(1.0)
        cr.rectangle(frame_x, frame_y, fw, fh)
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
        if state["tool"] in ("pen", "highlighter", "spray"):
            state["points"] = [pt]
        elif state["tool"] == "curve":
            state["points"] = [pt]
            state["curve_drag_mode"] = False
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
        if state["tool"] in ("pen", "highlighter", "spray", "curve"):
            state["points"].append(pt)
            if (
                state["tool"] == "curve"
                and not state["curve_waypoints"]
                and state["drag_start"] is not None
                and math.hypot(
                    pt["x"] - state["drag_start"]["x"], pt["y"] - state["drag_start"]["y"]
                )
                >= 8.0
            ):
                state["curve_drag_mode"] = True
        elif state["tool"] == "eraser":
            emit_json(
                {
                    "cmd": "erase",
                    "x": pt["x"],
                    "y": pt["y"],
                    "radius": max(state["width"] * 2.0, 10.0),
                }
            )
        elif state["tool"] == "crop" and state["drag_start"] is not None:
            a = state["drag_start"]
            _, _, pw, ph, valid = resolve_rect_bounds(
                {
                    "x": a["x"],
                    "y": a["y"],
                    "width": pt["x"] - a["x"],
                    "height": pt["y"] - a["y"],
                }
            )
            if valid:
                dim_label.set_label(f"{pw} × {ph} px (selecionando...)")
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
                            "style": state["line_style"],
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
        elif tool == "spray" and state["points"]:
            pts = list(state["points"])
            state["points"] = []
            emit_json(
                {
                    "cmd": "push_op",
                    "op": {
                        "Spray": {
                            "points": pts,
                            "color": state["color"],
                            "radius": max(state["width"] * 3.0, 8.0),
                        }
                    },
                }
            )
        elif tool == "curve":
            pts = list(state["points"])
            was_drag = state["curve_drag_mode"]
            state["points"] = []
            state["curve_drag_mode"] = False
            if was_drag and len(pts) >= 2:
                emit_json(
                    {
                        "cmd": "push_op",
                        "op": {
                            "CurvedLine": {
                                "points": pts,
                                "color": state["color"],
                                "width": state["width"],
                                "style": state["line_style"],
                            }
                        },
                    }
                )
            else:
                wps = state["curve_waypoints"]
                if (
                    len(wps) >= 2
                    and math.hypot(pt["x"] - wps[-1]["x"], pt["y"] - wps[-1]["y"]) <= 8.0
                ):
                    commit_curve_waypoints()
                elif not wps or math.hypot(pt["x"] - wps[-1]["x"], pt["y"] - wps[-1]["y"]) >= 2.0:
                    wps.append(pt)
        elif tool in ("line", "rect", "ellipse", "arrow", "double_arrow") and a is not None:
            kind_map = {
                "line": "Line",
                "rect": "Rectangle",
                "ellipse": "Ellipse",
                "arrow": "Arrow",
                "double_arrow": "DoubleArrow",
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
                            "style": state["line_style"],
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
        update_dim_label()
        area.queue_draw()

    def on_drag_cancel(_g, _sequence):
        state["dragging"] = False
        state["drag_start"] = None
        state["drag_current"] = None
        state["points"] = []
        state["curve_drag_mode"] = False
        update_dim_label()
        area.queue_draw()

    drag.connect("drag-begin", on_drag_begin)
    drag.connect("drag-update", on_drag_update)
    drag.connect("drag-end", on_drag_end)
    drag.connect("cancel", on_drag_cancel)
    area.add_controller(drag)

    motion = Gtk.EventControllerMotion()

    def on_motion(_m, x, y):
        if state["tool"] == "curve" and state["curve_waypoints"] and not state["dragging"]:
            state["hover_pt"] = to_image_point(area.get_width(), area.get_height(), x, y)
            area.queue_draw()

    def on_leave(_m):
        if state["hover_pt"] is not None:
            state["hover_pt"] = None
            area.queue_draw()

    motion.connect("motion", on_motion)
    motion.connect("leave", on_leave)
    area.add_controller(motion)

    rclick = Gtk.GestureClick(button=3)

    def on_right_click(_g, _n_press, _x, _y):
        if state["tool"] == "curve" and state["curve_waypoints"]:
            commit_curve_waypoints()

    rclick.connect("pressed", on_right_click)
    area.add_controller(rclick)

    def send_close_once():
        if not state["closed_by_host"] and not state["emitted_close"]:
            state["emitted_close"] = True
            emit_json({"cmd": "close"})

    key_ctrl = Gtk.EventControllerKey()

    def on_key_pressed(_ctrl, keyval, _keycode, mods):
        ctrl = bool(mods & Gdk.ModifierType.CONTROL_MASK)
        shift = bool(mods & Gdk.ModifierType.SHIFT_MASK)
        if keyval in (Gdk.KEY_Return, Gdk.KEY_KP_Enter) and state["tool"] == "curve" and state["curve_waypoints"]:
            commit_curve_waypoints()
            return True
        if keyval == Gdk.KEY_Escape:
            if state["curve_waypoints"]:
                state["curve_waypoints"] = []
                state["hover_pt"] = None
                area.queue_draw()
                return True
            win.close()
            return True
        if ctrl and keyval in (Gdk.KEY_z, Gdk.KEY_Z):
            trigger_host_cmd({"cmd": "redo" if shift else "undo"})
            return True
        if ctrl and keyval in (Gdk.KEY_y, Gdk.KEY_Y):
            trigger_host_cmd({"cmd": "redo"})
            return True
        if ctrl and keyval in (Gdk.KEY_c, Gdk.KEY_C):
            trigger_host_cmd({"cmd": "copy"})
            return True
        if ctrl and keyval in (Gdk.KEY_s, Gdk.KEY_S):
            trigger_host_cmd({"cmd": "save"})
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
                update_dim_label()
                status = payload.get("status")
                update_status_banner(status)
                area.queue_draw()
            except Exception:
                pass
        return False

    start_stdin_reader(app, on_line)
    win.present()


def main():
    mode = sys.argv[1]
    editor_png_bytes = b""
    if mode == "editor":
        img_fd = int(sys.argv[2])
        with os.fdopen(img_fd, "rb", closefd=True) as stream:
            editor_png_bytes = stream.read()

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
            w = int(sys.argv[3])
            h = int(sys.argv[4])
            dark = sys.argv[5] == "1"
            build_editor(application, editor_png_bytes, w, h, dark)

    app.connect("activate", on_activate)
    app.run(None)


if __name__ == "__main__":
    main()

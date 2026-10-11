mod audio;

use std::cell::{Cell, RefCell};
use std::process::Child;
use std::rc::Rc;
use std::time::Duration;

use gtk4::gdk;
use gtk4::glib;
use gtk4::pango;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box, Button, CheckButton, CssProvider, DropDown,
    GestureDrag, HeaderBar, Image, Label, Orientation, Paned, PolicyType, Scale, ScrolledWindow,
    SpinButton, StringList, ToggleButton,
};

use audio::{
    align_delay_ms, device_by_index, device_by_name, fmt_ms, load_snapshot, move_stream, rescue,
    set_default_sink, set_device_mute, set_device_volume, set_sink_port, set_stream_mute,
    set_stream_volume, start_loopback, stop_child, stop_pid, Device, Loopback, Snapshot, Stream,
};

const STYLE_CSS: &str = include_str!("style.css");

struct Route {
    stream_index: u32,
    to_sink: String,
    child: Child,
}

struct Flags {
    building: Cell<bool>,
    dragging: Cell<bool>,
}

struct DeviceRow {
    name: String,
    scale: Scale,
    mute: ToggleButton,
    mute_img: Image,
    vol_label: Label,
    port: DropDown,
    port_names: Vec<String>,
    latency: Label,
    state_badge: Label,
    default_badge: Label,
    default_btn: Button,
    card: Box,
}

struct ExtraRow {
    sink_name: String,
    enable: CheckButton,
    delay: SpinButton,
    whole: CheckButton,
    hint: Label,
    item_box: Box,
}

struct StreamRow {
    index: u32,
    scale: Scale,
    mute: ToggleButton,
    mute_img: Image,
    vol_label: Label,
    output: DropDown,
    output_selected: Rc<Cell<u32>>,
    extras: Vec<ExtraRow>,
}

struct Ui {
    _window: ApplicationWindow,
    devices_box: Box,
    streams_box: Box,
    status_pill: Label,
    status_bar_left: Label,
    status_bar_right: Label,
    banner_box: Box,
    banner_label: Label,
    banner_icon: Image,
    device_count_label: Label,
    stream_count_label: Label,
    device_rows: Vec<DeviceRow>,
    stream_rows: Vec<StreamRow>,
    routes: Vec<Route>,
    shape: String,
    flags: Rc<Flags>,
}

fn main() {
    let app = Application::builder()
        .application_id("in.devh.SoundRoute")
        .build();
    app.connect_activate(build);
    app.run();
}

fn build(app: &Application) {
    // Load and register CSS styles
    let provider = CssProvider::new();
    provider.load_from_string(STYLE_CSS);
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Sound Route")
        .default_width(1080)
        .default_height(720)
        .build();

    // Modern HeaderBar
    let header = HeaderBar::new();
    header.set_show_title_buttons(true);

    let title_box = Box::new(Orientation::Vertical, 0);
    let title_label = Label::new(Some("Sound Route"));
    title_label.add_css_class("header-title");
    let subtitle_label = Label::new(Some("PipeWire Audio Router & Mixer"));
    subtitle_label.add_css_class("header-subtitle");
    title_box.append(&title_label);
    title_box.append(&subtitle_label);
    header.set_title_widget(Some(&title_box));

    let status_pill = Label::new(Some("Ready"));
    status_pill.add_css_class("status-pill");
    header.pack_end(&status_pill);

    let refresh_btn = Button::from_icon_name("view-refresh-symbolic");
    refresh_btn.set_tooltip_text(Some("Refresh audio graph and rescue disconnected streams"));
    refresh_btn.add_css_class("flat");
    header.pack_end(&refresh_btn);

    window.set_titlebar(Some(&header));

    let root = Box::new(Orientation::Vertical, 0);

    // Dismissable notification/error banner
    let banner_box = Box::new(Orientation::Horizontal, 8);
    banner_box.add_css_class("banner-box");
    banner_box.set_visible(false);

    let banner_icon = Image::from_icon_name("dialog-information-symbolic");
    banner_box.append(&banner_icon);

    let banner_label = Label::new(None);
    banner_label.set_xalign(0.0);
    banner_label.set_wrap(true);
    banner_label.set_hexpand(true);
    banner_box.append(&banner_label);

    let banner_close = Button::from_icon_name("window-close-symbolic");
    banner_close.add_css_class("flat");
    let banner_box_clone = banner_box.clone();
    banner_close.connect_clicked(move |_| {
        banner_box_clone.set_visible(false);
    });
    banner_box.append(&banner_close);
    root.append(&banner_box);

    // Main Columns Layout
    let paned = Paned::new(Orientation::Horizontal);
    paned.set_hexpand(true);
    paned.set_vexpand(true);
    paned.set_position(460);

    // Left Column: Outputs
    let devices_wrapper = Box::new(Orientation::Vertical, 8);
    devices_wrapper.set_margin_top(12);
    devices_wrapper.set_margin_bottom(12);
    devices_wrapper.set_margin_start(14);
    devices_wrapper.set_margin_end(8);

    let dev_head = Box::new(Orientation::Horizontal, 8);
    let dev_icon = Image::from_icon_name("audio-speakers-symbolic");
    let dev_title = Label::new(Some("Audio Outputs"));
    dev_title.add_css_class("section-title");
    let device_count_label = Label::new(Some("0"));
    device_count_label.add_css_class("section-count-badge");
    dev_head.append(&dev_icon);
    dev_head.append(&dev_title);
    dev_head.append(&device_count_label);
    devices_wrapper.append(&dev_head);

    let devices_box = Box::new(Orientation::Vertical, 0);
    devices_wrapper.append(&scrolled(&devices_box));
    paned.set_start_child(Some(&devices_wrapper));

    // Right Column: Streams
    let streams_wrapper = Box::new(Orientation::Vertical, 8);
    streams_wrapper.set_margin_top(12);
    streams_wrapper.set_margin_bottom(12);
    streams_wrapper.set_margin_start(8);
    streams_wrapper.set_margin_end(14);

    let stream_head = Box::new(Orientation::Horizontal, 8);
    let stream_icon = Image::from_icon_name("media-playback-start-symbolic");
    let stream_title = Label::new(Some("Playback Streams"));
    stream_title.add_css_class("section-title");
    let stream_count_label = Label::new(Some("0"));
    stream_count_label.add_css_class("section-count-badge");
    stream_head.append(&stream_icon);
    stream_head.append(&stream_title);
    stream_head.append(&stream_count_label);
    streams_wrapper.append(&stream_head);

    let streams_box = Box::new(Orientation::Vertical, 0);
    streams_wrapper.append(&scrolled(&streams_box));
    paned.set_end_child(Some(&streams_wrapper));

    root.append(&paned);

    // Bottom Status Bar
    let status_bar = Box::new(Orientation::Horizontal, 8);
    status_bar.add_css_class("status-bar");

    let status_dot = Label::new(Some("●"));
    status_dot.add_css_class("status-dot");
    let status_bar_left = Label::new(Some("PipeWire Connected"));
    status_bar_left.set_xalign(0.0);
    status_bar_left.set_hexpand(true);

    let status_bar_right = Label::new(Some("0 copies active"));
    status_bar_right.set_xalign(1.0);

    status_bar.append(&status_dot);
    status_bar.append(&status_bar_left);
    status_bar.append(&status_bar_right);
    root.append(&status_bar);

    window.set_child(Some(&root));

    let ui = Rc::new(RefCell::new(Ui {
        _window: window.clone(),
        devices_box,
        streams_box,
        status_pill,
        status_bar_left,
        status_bar_right,
        banner_box,
        banner_label,
        banner_icon,
        device_count_label,
        stream_count_label,
        device_rows: Vec::new(),
        stream_rows: Vec::new(),
        routes: Vec::new(),
        shape: String::new(),
        flags: Rc::new(Flags {
            building: Cell::new(false),
            dragging: Cell::new(false),
        }),
    }));

    // Manual Refresh button wiring
    let ui_refresh = ui.clone();
    refresh_btn.connect_clicked(move |_| {
        ui_refresh.borrow_mut().shape.clear();
        refresh(&ui_refresh);
    });

    let tick = ui.clone();
    glib::timeout_add_local(Duration::from_millis(1500), move || {
        refresh(&tick);
        glib::ControlFlow::Continue
    });

    let closing = ui.clone();
    window.connect_close_request(move |_| {
        shutdown(&closing);
        glib::Propagation::Proceed
    });

    refresh(&ui);
    window.present();
}

fn scrolled(child: &Box) -> ScrolledWindow {
    let scroll = ScrolledWindow::new();
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(child));
    scroll.set_hexpand(true);
    scroll.set_vexpand(true);
    scroll
}

fn set_banner(ui: &Rc<RefCell<Ui>>, is_error: bool, text: &str) {
    let ui_ref = ui.borrow();
    if text.is_empty() {
        ui_ref.banner_box.set_visible(false);
        return;
    }
    ui_ref.banner_box.remove_css_class("banner-error");
    ui_ref.banner_box.remove_css_class("banner-info");
    if is_error {
        ui_ref.banner_box.add_css_class("banner-error");
        ui_ref
            .banner_icon
            .set_icon_name(Some("dialog-warning-symbolic"));
    } else {
        ui_ref.banner_box.add_css_class("banner-info");
        ui_ref
            .banner_icon
            .set_icon_name(Some("dialog-information-symbolic"));
    }
    ui_ref.banner_label.set_text(text);
    ui_ref.banner_box.set_visible(true);
}

fn clear_banner(ui: &Rc<RefCell<Ui>>) {
    ui.borrow().banner_box.set_visible(false);
}

fn refresh(ui: &Rc<RefCell<Ui>>) {
    let flags = ui.borrow().flags.clone();
    if flags.dragging.get() {
        return;
    }
    let snap = match load_snapshot() {
        Ok(snap) => snap,
        Err(e) => {
            set_banner(ui, true, &format!("Failed to query PipeWire: {e}"));
            return;
        }
    };

    reap_routes(ui, &snap);
    drop_routes_to_missing_devices(ui, &snap);
    let notes = rescue(&snap).unwrap_or_else(|e| vec![e]);
    let snap = if notes.is_empty() {
        snap
    } else {
        ui.borrow_mut().shape.clear();
        match load_snapshot() {
            Ok(snap) => snap,
            Err(e) => {
                set_banner(ui, true, &e);
                return;
            }
        }
    };

    let shape = shape_key(&snap);
    let rebuild = ui.borrow().shape != shape;
    if rebuild {
        rebuild_ui(ui, &snap);
        ui.borrow_mut().shape = shape;
    } else {
        sync_widgets(ui, &snap);
    }

    let managed = ui.borrow().routes.len();
    let ui_ref = ui.borrow();
    ui_ref
        .device_count_label
        .set_text(&snap.devices.len().to_string());
    ui_ref
        .stream_count_label
        .set_text(&snap.streams.len().to_string());

    ui_ref.status_pill.set_text(&format!(
        "{} Outputs • {} Streams",
        snap.devices.len(),
        snap.streams.len()
    ));

    ui_ref.status_bar_left.set_text(&format!(
        "PipeWire Audio Graph • {} outputs, {} playing apps",
        snap.devices.len(),
        snap.streams.len()
    ));
    ui_ref.status_bar_right.set_text(&if managed == 1 {
        "1 copy route active".into()
    } else {
        format!("{managed} copy routes active")
    });

    if !notes.is_empty() {
        drop(ui_ref);
        set_banner(ui, false, &notes.join(" • "));
    }
}

fn drop_routes_to_missing_devices(ui: &Rc<RefCell<Ui>>, snap: &Snapshot) {
    let mut ui_ref = ui.borrow_mut();
    let mut index = 0;
    while index < ui_ref.routes.len() {
        let gone = !snap.devices.iter().any(|d| d.name == ui_ref.routes[index].to_sink);
        if gone {
            let mut route = ui_ref.routes.remove(index);
            stop_child(&mut route.child);
        } else {
            index += 1;
        }
    }
}

fn shape_key(snap: &Snapshot) -> String {
    let mut parts = Vec::new();
    for d in &snap.devices {
        parts.push(format!(
            "d:{}:{}:{}",
            d.index,
            d.name,
            d.ports
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    for s in &snap.streams {
        parts.push(format!("s:{}:{}:{}", s.index, s.serial, s.sink));
    }
    for o in &snap.orphans {
        parts.push(format!("o:{}", o.pid));
    }
    parts.join("|")
}

fn icon_for_device(name: &str, desc: &str) -> &'static str {
    let s = format!("{} {}", name.to_lowercase(), desc.to_lowercase());
    if s.contains("headphone") || s.contains("headset") || s.contains("earbud") || s.contains("airpod") {
        "audio-headphones-symbolic"
    } else if s.contains("bluez") || s.contains("bluetooth") {
        "audio-headphones-symbolic"
    } else if s.contains("hdmi") || s.contains("displayport") {
        "video-display-symbolic"
    } else if s.contains("usb") {
        "audio-card-symbolic"
    } else {
        "audio-speakers-symbolic"
    }
}

fn icon_for_stream(title: &str, detail: &str) -> &'static str {
    let s = format!("{} {}", title.to_lowercase(), detail.to_lowercase());
    if s.contains("firefox") || s.contains("chrome") || s.contains("browser") || s.contains("brave") || s.contains("edge") {
        "web-browser-symbolic"
    } else if s.contains("spotify") || s.contains("music") || s.contains("rhythmbox") || s.contains("vlc") || s.contains("mpv") {
        "media-playback-start-symbolic"
    } else if s.contains("discord") || s.contains("telegram") || s.contains("slack") || s.contains("zoom") || s.contains("teams") {
        "call-start-symbolic"
    } else {
        "audio-x-generic-symbolic"
    }
}

fn rebuild_ui(ui: &Rc<RefCell<Ui>>, snap: &Snapshot) {
    let flags = ui.borrow().flags.clone();
    flags.building.set(true);

    let devices_box = ui.borrow().devices_box.clone();
    let streams_box = ui.borrow().streams_box.clone();
    while let Some(child) = devices_box.first_child() {
        devices_box.remove(&child);
    }
    while let Some(child) = streams_box.first_child() {
        streams_box.remove(&child);
    }

    let mut device_rows = Vec::new();
    for device in &snap.devices {
        device_rows.push(build_device_row(ui, snap, device, &devices_box));
    }

    let mut stream_rows = Vec::new();
    if snap.streams.is_empty() {
        let empty_box = Box::new(Orientation::Vertical, 6);
        empty_box.add_css_class("empty-state");
        empty_box.set_halign(gtk4::Align::Center);
        empty_box.set_valign(gtk4::Align::Center);
        empty_box.set_vexpand(true);

        let icon = Image::from_icon_name("audio-x-generic-symbolic");
        icon.set_pixel_size(48);
        let title = Label::new(Some("No Active Audio Streams"));
        title.add_css_class("empty-title");
        let desc = Label::new(Some(
            "Play audio from any browser, music player, or game to route and broadcast.",
        ));
        desc.add_css_class("empty-desc");
        desc.set_wrap(true);
        desc.set_justify(gtk4::Justification::Center);

        empty_box.append(&icon);
        empty_box.append(&title);
        empty_box.append(&desc);
        streams_box.append(&empty_box);
    } else {
        for stream in &snap.streams {
            stream_rows.push(build_stream_row(ui, snap, stream, &streams_box));
        }
    }

    let tracked: Vec<u32> = ui.borrow().routes.iter().map(|r| r.child.id()).collect();
    let orphans: Vec<_> = snap
        .orphans
        .iter()
        .filter(|o| !tracked.contains(&o.pid))
        .cloned()
        .collect();
    if !orphans.is_empty() {
        let orphan_card = Box::new(Orientation::Vertical, 8);
        orphan_card.add_css_class("orphan-card");

        let o_head = Box::new(Orientation::Horizontal, 8);
        let o_icon = Image::from_icon_name("dialog-warning-symbolic");
        let o_title = Label::new(Some("Lingering Background Copies"));
        o_title.add_css_class("card-title");
        o_title.set_hexpand(true);
        o_title.set_xalign(0.0);

        let stop_all = Button::with_label("Stop All");
        stop_all.add_css_class("destructive-action");
        stop_all.add_css_class("pill-button");
        let ui_stop_all = ui.clone();
        let pids: Vec<u32> = orphans.iter().map(|o| o.pid).collect();
        stop_all.connect_clicked(move |_| {
            for &pid in &pids {
                let _ = stop_pid(pid);
            }
            ui_stop_all.borrow_mut().shape.clear();
            refresh(&ui_stop_all);
        });

        o_head.append(&o_icon);
        o_head.append(&o_title);
        o_head.append(&stop_all);
        orphan_card.append(&o_head);

        for orphan in orphans {
            let row = Box::new(Orientation::Horizontal, 8);
            let text = Label::new(Some(&format!("{} (PID {})", orphan.label, orphan.pid)));
            text.set_xalign(0.0);
            text.set_hexpand(true);
            text.add_css_class("card-subtitle");

            let stop = Button::with_label("Stop");
            stop.add_css_class("pill-button");
            let ui_stop = ui.clone();
            let pid = orphan.pid;
            stop.connect_clicked(move |_| match stop_pid(pid) {
                Ok(()) => {
                    clear_banner(&ui_stop);
                    ui_stop.borrow_mut().shape.clear();
                    refresh(&ui_stop);
                }
                Err(e) => set_banner(&ui_stop, true, &e),
            });
            row.append(&text);
            row.append(&stop);
            orphan_card.append(&row);
        }
        streams_box.append(&orphan_card);
    }

    let mut ui_ref = ui.borrow_mut();
    ui_ref.device_rows = device_rows;
    ui_ref.stream_rows = stream_rows;
    flags.building.set(false);
}

fn build_device_row(ui: &Rc<RefCell<Ui>>, snap: &Snapshot, device: &Device, parent: &Box) -> DeviceRow {
    let card = Box::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    if device.name == snap.default_sink {
        card.add_css_class("card-default");
    }

    // Header row: Icon, Name/Desc, Badges, Default button
    let head = Box::new(Orientation::Horizontal, 8);
    let dev_icon = Image::from_icon_name(icon_for_device(&device.name, &device.description));
    dev_icon.set_pixel_size(24);
    head.append(&dev_icon);

    let title_box = Box::new(Orientation::Vertical, 2);
    title_box.set_hexpand(true);
    let title = Label::new(Some(&device.description));
    title.set_xalign(0.0);
    title.add_css_class("card-title");
    title.set_ellipsize(pango::EllipsizeMode::End);
    title.set_tooltip_text(Some(&device.description));

    let latency = Label::new(Some(&device_subtitle(snap, device)));
    latency.set_xalign(0.0);
    latency.add_css_class("card-subtitle");
    title_box.append(&title);
    title_box.append(&latency);
    head.append(&title_box);

    let badges_box = Box::new(Orientation::Horizontal, 6);
    let state_badge = Label::new(Some(&device.state));
    state_badge.add_css_class("badge");
    if device.state == "RUNNING" {
        state_badge.add_css_class("badge-state-running");
    } else {
        state_badge.add_css_class("badge-state-idle");
    }
    badges_box.append(&state_badge);

    let default_badge = Label::new(Some("DEFAULT"));
    default_badge.add_css_class("badge");
    default_badge.add_css_class("badge-default");
    default_badge.set_visible(device.name == snap.default_sink);
    badges_box.append(&default_badge);

    let default_btn = Button::with_label("Make Default");
    default_btn.add_css_class("pill-button");
    default_btn.add_css_class("pill-button-accent");
    default_btn.set_visible(device.name != snap.default_sink);
    let name = device.name.clone();
    let ui_d = ui.clone();
    default_btn.connect_clicked(move |_| {
        let ui_action = ui_d.clone();
        if let Err(e) = set_default_sink(&name) {
            set_banner(&ui_action, true, &e);
        } else {
            clear_banner(&ui_action);
            ui_action.borrow_mut().shape.clear();
            refresh(&ui_action);
        }
    });
    badges_box.append(&default_btn);

    head.append(&badges_box);
    card.append(&head);

    // Volume Strip: Mute button, Slider, Value Label
    let vol_row = Box::new(Orientation::Horizontal, 8);
    let mute = ToggleButton::new();
    mute.add_css_class("flat");
    mute.set_active(device.muted);
    mute.set_tooltip_text(Some(if device.muted { "Unmute output" } else { "Mute output" }));

    let mute_img = Image::from_icon_name(if device.muted {
        "audio-volume-muted-symbolic"
    } else {
        "audio-volume-high-symbolic"
    });
    mute.set_child(Some(&mute_img));

    let flags = ui.borrow().flags.clone();
    let flags_m = flags.clone();
    let name_m = device.name.clone();
    let ui_m = ui.clone();
    let mute_img_clone = mute_img.clone();
    mute.connect_toggled(move |btn| {
        if flags_m.building.get() {
            return;
        }
        let active = btn.is_active();
        mute_img_clone.set_icon_name(Some(if active {
            "audio-volume-muted-symbolic"
        } else {
            "audio-volume-high-symbolic"
        }));
        btn.set_tooltip_text(Some(if active { "Unmute output" } else { "Mute output" }));
        if let Err(e) = set_device_mute(&name_m, active) {
            set_banner(&ui_m, true, &e);
        }
    });
    vol_row.append(&mute);

    let scale = volume_scale(device.volume);
    vol_row.append(&scale);

    let vol_label = Label::new(Some(&format!("{:.0}%", device.volume * 100.0)));
    vol_label.add_css_class("volume-val-label");
    vol_row.append(&vol_label);

    let flags_v = ui.borrow().flags.clone();
    let name_v = device.name.clone();
    let ui_vol = ui.clone();
    let vol_label_clone = vol_label.clone();
    wire_volume(&scale, &flags_v, vol_label_clone, move |value| {
        if let Err(e) = set_device_volume(&name_v, value) {
            set_banner(&ui_vol, true, &e);
        }
    });
    card.append(&vol_row);

    // Port Selector (if hardware has multiple switchable ports)
    let (port, port_names) = port_dropdown(device);
    if port_names.len() > 1 {
        let port_row = Box::new(Orientation::Horizontal, 8);
        port_row.set_margin_top(2);
        let port_icon = Image::from_icon_name("audio-card-symbolic");
        let port_label = Label::new(Some("Port:"));
        port_label.add_css_class("card-subtitle");
        port.set_hexpand(true);

        port_row.append(&port_icon);
        port_row.append(&port_label);
        port_row.append(&port);

        let flags_p = flags.clone();
        let name_p = device.name.clone();
        let names = port_names.clone();
        let previous = Rc::new(Cell::new(port.selected()));
        let ui_p = ui.clone();
        port.connect_selected_notify(move |dd| {
            if flags_p.building.get() {
                return;
            }
            let new = dd.selected();
            let old = previous.get();
            if new == old || new == gtk4::INVALID_LIST_POSITION {
                return;
            }
            let Some(port_name) = names.get(new as usize) else {
                return;
            };
            previous.set(new);
            if let Err(e) = set_sink_port(&name_p, port_name) {
                set_banner(&ui_p, true, &e);
            } else {
                clear_banner(&ui_p);
            }
        });
        card.append(&port_row);
    }

    parent.append(&card);

    DeviceRow {
        name: device.name.clone(),
        scale,
        mute,
        mute_img,
        vol_label,
        port,
        port_names,
        latency,
        state_badge,
        default_badge,
        default_btn,
        card,
    }
}

fn build_stream_row(ui: &Rc<RefCell<Ui>>, snap: &Snapshot, stream: &Stream, parent: &Box) -> StreamRow {
    let card = Box::new(Orientation::Vertical, 8);
    card.add_css_class("card");

    // Header row: Stream Icon, Title, Detail, Badges
    let head = Box::new(Orientation::Horizontal, 8);
    let app_icon = Image::from_icon_name(icon_for_stream(&stream.title, &stream.detail));
    app_icon.set_pixel_size(24);
    head.append(&app_icon);

    let title_box = Box::new(Orientation::Vertical, 2);
    title_box.set_hexpand(true);
    let title = Label::new(Some(&stream.title));
    title.set_xalign(0.0);
    title.add_css_class("card-title");
    title.set_ellipsize(pango::EllipsizeMode::End);

    let detail_text = if stream.detail.is_empty() {
        format!("Stream index {}", stream.index)
    } else {
        stream.detail.clone()
    };
    let detail = Label::new(Some(&detail_text));
    detail.set_xalign(0.0);
    detail.add_css_class("card-subtitle");
    detail.set_ellipsize(pango::EllipsizeMode::End);

    title_box.append(&title);
    title_box.append(&detail);
    head.append(&title_box);

    if stream.detail.contains("paused") {
        let badge = Label::new(Some("PAUSED"));
        badge.add_css_class("badge");
        badge.add_css_class("badge-paused");
        head.append(&badge);
    }
    if stream.stranded {
        let badge = Label::new(Some("UNLINKED"));
        badge.add_css_class("badge");
        badge.add_css_class("badge-stranded");
        head.append(&badge);
    }

    card.append(&head);

    // Stream Volume Strip
    let vol_row = Box::new(Orientation::Horizontal, 8);
    let mute = ToggleButton::new();
    mute.add_css_class("flat");
    mute.set_active(stream.muted);
    mute.set_tooltip_text(Some(if stream.muted { "Unmute stream" } else { "Mute stream" }));

    let mute_img = Image::from_icon_name(if stream.muted {
        "audio-volume-muted-symbolic"
    } else {
        "audio-volume-high-symbolic"
    });
    mute.set_child(Some(&mute_img));

    let flags = ui.borrow().flags.clone();
    let flags_m = flags.clone();
    let index_m = stream.index;
    let ui_m = ui.clone();
    let mute_img_clone = mute_img.clone();
    mute.connect_toggled(move |btn| {
        if flags_m.building.get() {
            return;
        }
        let active = btn.is_active();
        mute_img_clone.set_icon_name(Some(if active {
            "audio-volume-muted-symbolic"
        } else {
            "audio-volume-high-symbolic"
        }));
        btn.set_tooltip_text(Some(if active { "Unmute stream" } else { "Mute stream" }));
        if let Err(e) = set_stream_mute(index_m, active) {
            set_banner(&ui_m, true, &e);
        }
    });
    vol_row.append(&mute);

    let scale = volume_scale(stream.volume);
    vol_row.append(&scale);

    let vol_label = Label::new(Some(&format!("{:.0}%", stream.volume * 100.0)));
    vol_label.add_css_class("volume-val-label");
    vol_row.append(&vol_label);

    let flags_v = ui.borrow().flags.clone();
    let index_v = stream.index;
    let ui_v = ui.clone();
    let vol_label_clone = vol_label.clone();
    wire_volume(&scale, &flags_v, vol_label_clone, move |value| {
        if let Err(e) = set_stream_volume(index_v, value) {
            set_banner(&ui_v, true, &e);
        }
    });
    card.append(&vol_row);

    // Primary Output Selector Row
    let out_row = Box::new(Orientation::Horizontal, 8);
    out_row.set_margin_top(4);
    let out_label = Label::new(Some("Primary Output:"));
    out_label.add_css_class("card-subtitle");

    let names: Vec<String> = snap.devices.iter().map(|d| d.name.clone()).collect();
    let labels: Vec<String> = snap
        .devices
        .iter()
        .map(|d| format!("{} · {}", d.description, fmt_ms(d.latency_ms)))
        .collect();
    let output = dropdown_from(&labels);
    if let Some(pos) = snap.devices.iter().position(|d| d.index == stream.sink) {
        output.set_selected(pos as u32);
    }
    output.set_hexpand(true);

    let previous = Rc::new(Cell::new(output.selected()));
    let flags_o = flags.clone();
    let ui_o = ui.clone();
    let stream_index = stream.index;
    let output_selected = previous.clone();
    output.connect_selected_notify(move |dd| {
        if flags_o.building.get() {
            return;
        }
        let new = dd.selected();
        let old = previous.get();
        if new == old || new == gtk4::INVALID_LIST_POSITION {
            return;
        }
        let Some(sink_name) = names.get(new as usize).cloned() else {
            return;
        };
        previous.set(new);
        let ui_action = ui_o.clone();
        match move_stream(stream_index, &sink_name) {
            Ok(()) => {
                clear_banner(&ui_action);
                ui_action.borrow_mut().shape.clear();
                restart_whole_output_routes(&ui_action, stream_index, &sink_name);
            }
            Err(e) => set_banner(&ui_action, true, &e),
        }
    });

    out_row.append(&out_label);
    out_row.append(&output);
    card.append(&out_row);

    // Multi-Output Fan-Out Sub-Panel
    let fan_panel = Box::new(Orientation::Vertical, 6);
    fan_panel.add_css_class("sub-panel");

    let fan_head = Box::new(Orientation::Horizontal, 8);
    let fan_title = Label::new(Some("Fan-Out Copies (Broadcast)"));
    fan_title.add_css_class("sub-panel-title");
    fan_title.set_hexpand(true);
    fan_title.set_xalign(0.0);
    fan_head.append(&fan_title);

    let mut extras = Vec::new();
    if snap.devices.len() > 1 {
        let align_all = Button::with_label("Auto-Sync All");
        align_all.add_css_class("pill-button");
        align_all.add_css_class("pill-button-accent");
        align_all.set_tooltip_text(Some("Align all enabled copies to match the primary output latency"));
        let ui_a = ui.clone();
        let stream_index_a = stream.index;
        align_all.connect_clicked(move |_| align_checked(&ui_a, stream_index_a));
        fan_head.append(&align_all);
    }
    fan_panel.append(&fan_head);

    if snap.devices.len() <= 1 {
        let single_hint = Label::new(Some(
            "Connect another audio output (headphones, Bluetooth, HDMI) to enable fan-out copying.",
        ));
        single_hint.set_xalign(0.0);
        single_hint.add_css_class("hint-text");
        fan_panel.append(&single_hint);
    } else {
        for device in &snap.devices {
            if device.index == stream.sink {
                continue;
            }
            extras.push(build_extra(ui, snap, stream, device, &fan_panel));
        }
    }

    card.append(&fan_panel);
    parent.append(&card);

    StreamRow {
        index: stream.index,
        scale,
        mute,
        mute_img,
        vol_label,
        output,
        output_selected,
        extras,
    }
}

fn build_extra(
    ui: &Rc<RefCell<Ui>>,
    snap: &Snapshot,
    stream: &Stream,
    device: &Device,
    parent: &Box,
) -> ExtraRow {
    let item_box = Box::new(Orientation::Vertical, 4);
    item_box.add_css_class("route-extra-item");

    let already = ui
        .borrow()
        .routes
        .iter()
        .any(|r| r.stream_index == stream.index && r.to_sink == device.name);
    if already {
        item_box.add_css_class("active-copy");
    }

    // Top line: Checkbox with target name & latency
    let top_line = Box::new(Orientation::Horizontal, 8);
    let dev_icon = Image::from_icon_name(icon_for_device(&device.name, &device.description));
    top_line.append(&dev_icon);

    let enable = CheckButton::with_label(&device.description);
    enable.set_active(already);
    enable.set_hexpand(true);
    top_line.append(&enable);

    let lat_badge = Label::new(Some(&fmt_ms(device.latency_ms)));
    lat_badge.add_css_class("badge");
    lat_badge.add_css_class("badge-latency");
    top_line.append(&lat_badge);
    item_box.append(&top_line);

    // Controls line: Delay Spin, Auto Sync, Whole output checkbox
    let ctrl_line = Box::new(Orientation::Horizontal, 8);
    ctrl_line.set_margin_start(28);

    let sync_label = Label::new(Some("Delay:"));
    sync_label.add_css_class("card-subtitle");
    ctrl_line.append(&sync_label);

    let delay = SpinButton::with_range(0.0, 2000.0, 1.0);
    delay.set_digits(0);
    let main_latency = device_by_index(snap, stream.sink).and_then(|d| d.latency_ms);
    let auto_val = align_delay_ms(main_latency);
    delay.set_value(auto_val.round());
    delay.set_tooltip_text(Some(
        "Buffer delay in ms so this copy lines up in sync with the primary output.",
    ));
    ctrl_line.append(&delay);

    let ms_lbl = Label::new(Some("ms"));
    ms_lbl.add_css_class("card-subtitle");
    ctrl_line.append(&ms_lbl);

    let auto_btn = Button::with_label("Auto");
    auto_btn.add_css_class("pill-button");
    auto_btn.set_tooltip_text(Some("Calculate delay from primary output's measured latency"));
    ctrl_line.append(&auto_btn);

    let whole = CheckButton::with_label("Mirror master mix");
    whole.set_tooltip_text(Some(
        "Off: route only this stream. On: capture everything currently playing on the master output.",
    ));
    ctrl_line.append(&whole);

    item_box.append(&ctrl_line);

    let hint = Label::new(Some(&extra_hint(snap, stream, device)));
    hint.set_xalign(0.0);
    hint.set_margin_start(28);
    hint.add_css_class("hint-text");
    item_box.append(&hint);

    // Wiring Enable toggle (Instant toggle, zero annoying popups!)
    let flags = ui.borrow().flags.clone();
    let ui_e = ui.clone();
    let stream_index = stream.index;
    let to_sink = device.name.clone();
    let delay_for_toggle = delay.clone();
    let whole_for_toggle = whole.clone();
    let item_box_clone = item_box.clone();

    enable.connect_toggled(move |btn| {
        if flags.building.get() {
            return;
        }
        let is_active = btn.is_active();
        if is_active {
            item_box_clone.add_css_class("active-copy");
        } else {
            item_box_clone.remove_css_class("active-copy");
        }

        if !is_active {
            stop_route(&ui_e, stream_index, &to_sink);
            return;
        }

        let snap = match load_snapshot() {
            Ok(s) => s,
            Err(e) => {
                set_banner(&ui_e, true, &e);
                return;
            }
        };
        let Some(stream) = snap.streams.iter().find(|s| s.index == stream_index) else {
            set_banner(&ui_e, true, "That stream is no longer playing.");
            return;
        };
        let from_sink = device_by_index(&snap, stream.sink)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        let serial = stream.serial.clone();
        let delay_ms = delay_for_toggle.value();
        let whole_output = whole_for_toggle.is_active();

        match spawn_route(
            &ui_e,
            stream_index,
            &serial,
            &from_sink,
            &to_sink,
            whole_output,
            delay_ms,
        ) {
            Ok(()) => clear_banner(&ui_e),
            Err(e) => {
                flags.building.set(true);
                btn.set_active(false);
                item_box_clone.remove_css_class("active-copy");
                flags.building.set(false);
                set_banner(&ui_e, true, &e);
            }
        }
    });

    // Auto button wiring
    let ui_auto = ui.clone();
    let to_sink_auto = device.name.clone();
    let delay_auto = delay.clone();
    let whole_auto = whole.clone();
    let stream_index_auto = stream.index;
    auto_btn.connect_clicked(move |_| {
        let snap = match load_snapshot() {
            Ok(s) => s,
            Err(e) => {
                set_banner(&ui_auto, true, &e);
                return;
            }
        };
        let Some(stream) = snap.streams.iter().find(|s| s.index == stream_index_auto) else {
            set_banner(&ui_auto, true, "That stream is no longer playing.");
            return;
        };
        let ms = align_delay_ms(device_by_index(&snap, stream.sink).and_then(|d| d.latency_ms));
        delay_auto.set_value(ms.round());
        let active = ui_auto
            .borrow()
            .routes
            .iter()
            .any(|r| r.stream_index == stream_index_auto && r.to_sink == to_sink_auto);
        if active {
            apply_delay(
                &ui_auto,
                stream_index_auto,
                &to_sink_auto,
                whole_auto.is_active(),
                ms,
            );
        } else {
            set_banner(
                &ui_auto,
                false,
                &format!(
                    "Sync delay set to {:.0} ms. Turn the copy on to activate.",
                    ms.round()
                ),
            );
        }
    });

    // Delay Spin value change wiring
    let ui_spin = ui.clone();
    let to_sink_spin = device.name.clone();
    let whole_spin = whole.clone();
    let stream_index_spin = stream.index;
    delay.connect_value_changed(move |spin| {
        let active = ui_spin
            .borrow()
            .routes
            .iter()
            .any(|r| r.stream_index == stream_index_spin && r.to_sink == to_sink_spin);
        if active {
            apply_delay(
                &ui_spin,
                stream_index_spin,
                &to_sink_spin,
                whole_spin.is_active(),
                spin.value(),
            );
        }
    });

    parent.append(&item_box);

    ExtraRow {
        sink_name: device.name.clone(),
        enable,
        delay,
        whole,
        hint,
        item_box,
    }
}

fn spawn_route(
    ui: &Rc<RefCell<Ui>>,
    stream_index: u32,
    stream_serial: &str,
    from_sink: &str,
    to_sink: &str,
    whole_output: bool,
    delay_ms: f64,
) -> Result<(), String> {
    if stream_serial.is_empty() && !whole_output {
        return Err("This stream has no PipeWire serial. Turn on 'Mirror master mix'.".into());
    }
    if from_sink.is_empty() {
        return Err("The primary output is not available.".into());
    }
    stop_route(ui, stream_index, to_sink);
    let node_name = format!("sound-route-{stream_serial}-to-{to_sink}");
    let child = start_loopback(&Loopback {
        node_name,
        stream_serial: stream_serial.to_string(),
        from_sink: from_sink.to_string(),
        whole_output,
        to_sink: to_sink.to_string(),
        delay_ms,
    })?;
    ui.borrow_mut().routes.push(Route {
        stream_index,
        to_sink: to_sink.to_string(),
        child,
    });
    Ok(())
}

fn stop_route(ui: &Rc<RefCell<Ui>>, stream_index: u32, to_sink: &str) {
    let mut ui_ref = ui.borrow_mut();
    if let Some(pos) = ui_ref
        .routes
        .iter()
        .position(|r| r.stream_index == stream_index && r.to_sink == to_sink)
    {
        let mut route = ui_ref.routes.remove(pos);
        stop_child(&mut route.child);
    }
}

fn apply_delay(
    ui: &Rc<RefCell<Ui>>,
    stream_index: u32,
    to_sink: &str,
    whole_output: bool,
    delay_ms: f64,
) {
    let active = ui
        .borrow()
        .routes
        .iter()
        .any(|r| r.stream_index == stream_index && r.to_sink == to_sink);
    if !active {
        return;
    }
    let snap = match load_snapshot() {
        Ok(s) => s,
        Err(e) => {
            set_banner(ui, true, &e);
            return;
        }
    };
    let Some(stream) = snap.streams.iter().find(|s| s.index == stream_index) else {
        set_banner(ui, true, "That stream is no longer playing.");
        return;
    };
    let from_sink = device_by_index(&snap, stream.sink)
        .map(|d| d.name.clone())
        .unwrap_or_default();
    let serial = stream.serial.clone();
    match spawn_route(ui, stream_index, &serial, &from_sink, to_sink, whole_output, delay_ms) {
        Ok(()) => clear_banner(ui),
        Err(e) => set_banner(ui, true, &e),
    }
}

fn align_checked(ui: &Rc<RefCell<Ui>>, stream_index: u32) {
    let snap = match load_snapshot() {
        Ok(s) => s,
        Err(e) => {
            set_banner(ui, true, &e);
            return;
        }
    };
    let Some(stream) = snap.streams.iter().find(|s| s.index == stream_index) else {
        return;
    };
    let ms = align_delay_ms(device_by_index(&snap, stream.sink).and_then(|d| d.latency_ms));
    let targets: Vec<(String, bool)> = {
        let ui_ref = ui.borrow();
        let Some(row) = ui_ref.stream_rows.iter().find(|r| r.index == stream_index) else {
            return;
        };
        row.extras
            .iter()
            .filter(|e| e.enable.is_active())
            .map(|e| (e.sink_name.clone(), e.whole.is_active()))
            .collect()
    };
    if targets.is_empty() {
        set_banner(ui, false, "No fan-out output is currently enabled.");
        return;
    }
    let from = device_by_index(&snap, stream.sink).map(|d| d.name.clone()).unwrap_or_default();
    for (to_sink, whole) in targets {
        if let Some(row) = ui.borrow().stream_rows.iter().find(|r| r.index == stream_index) {
            if let Some(extra) = row.extras.iter().find(|e| e.sink_name == to_sink) {
                extra.delay.set_value(ms.round());
            }
        }
        if let Err(e) = spawn_route(ui, stream_index, &stream.serial, &from, &to_sink, whole, ms) {
            set_banner(ui, true, &e);
            return;
        }
    }
    clear_banner(ui);
}

fn restart_whole_output_routes(ui: &Rc<RefCell<Ui>>, stream_index: u32, new_sink: &str) {
    let legs: Vec<(String, f64)> = ui
        .borrow()
        .stream_rows
        .iter()
        .find(|r| r.index == stream_index)
        .map(|row| {
            row.extras
                .iter()
                .filter(|e| e.enable.is_active() && e.whole.is_active())
                .map(|e| (e.sink_name.clone(), e.delay.value()))
                .collect()
        })
        .unwrap_or_default();
    let serial = load_snapshot()
        .ok()
        .and_then(|s| s.streams.iter().find(|st| st.index == stream_index).map(|st| st.serial.clone()))
        .unwrap_or_default();
    for (to_sink, delay_ms) in legs {
        if to_sink == new_sink {
            stop_route(ui, stream_index, &to_sink);
            continue;
        }
        if let Err(e) = spawn_route(ui, stream_index, &serial, new_sink, &to_sink, true, delay_ms) {
            set_banner(ui, true, &e);
        }
    }
}

fn reap_routes(ui: &Rc<RefCell<Ui>>, _snap: &Snapshot) {
    let mut failed = false;
    {
        let mut ui_ref = ui.borrow_mut();
        let mut i = 0;
        while i < ui_ref.routes.len() {
            match ui_ref.routes[i].child.try_wait() {
                Ok(Some(_)) => {
                    ui_ref.routes.remove(i);
                    failed = true;
                }
                _ => i += 1,
            }
        }
    }
    if failed {
        set_banner(
            ui,
            true,
            "A copy stopped. PipeWire would not link that stream. Try 'Mirror master mix'.",
        );
        ui.borrow_mut().shape.clear();
    }
}

fn sync_widgets(ui: &Rc<RefCell<Ui>>, snap: &Snapshot) {
    let flags = ui.borrow().flags.clone();
    flags.building.set(true);
    let ui_ref = ui.borrow();
    for row in &ui_ref.device_rows {
        let Some(device) = device_by_name(snap, &row.name) else {
            continue;
        };
        if !flags.dragging.get() {
            row.scale.set_value(device.volume * 100.0);
            row.vol_label
                .set_text(&format!("{:.0}%", device.volume * 100.0));
        }
        row.mute.set_active(device.muted);
        row.mute_img.set_icon_name(Some(if device.muted {
            "audio-volume-muted-symbolic"
        } else {
            "audio-volume-high-symbolic"
        }));
        row.latency.set_text(&device_subtitle(snap, device));
        row.state_badge.set_text(&device.state);
        row.state_badge.remove_css_class("badge-state-running");
        row.state_badge.remove_css_class("badge-state-idle");
        if device.state == "RUNNING" {
            row.state_badge.add_css_class("badge-state-running");
        } else {
            row.state_badge.add_css_class("badge-state-idle");
        }

        let is_default = device.name == snap.default_sink;
        row.default_badge.set_visible(is_default);
        row.default_btn.set_visible(!is_default);
        row.card.remove_css_class("card-default");
        if is_default {
            row.card.add_css_class("card-default");
        }

        if let Some(pos) = row.port_names.iter().position(|n| n == &device.active_port) {
            if row.port.selected() != pos as u32 {
                row.port.set_selected(pos as u32);
            }
        }
    }

    for row in &ui_ref.stream_rows {
        let Some(stream) = snap.streams.iter().find(|s| s.index == row.index) else {
            continue;
        };
        if !flags.dragging.get() {
            row.scale.set_value(stream.volume * 100.0);
            row.vol_label
                .set_text(&format!("{:.0}%", stream.volume * 100.0));
        }
        row.mute.set_active(stream.muted);
        row.mute_img.set_icon_name(Some(if stream.muted {
            "audio-volume-muted-symbolic"
        } else {
            "audio-volume-high-symbolic"
        }));

        if let Some(pos) = snap.devices.iter().position(|d| d.index == stream.sink) {
            row.output_selected.set(pos as u32);
            if row.output.selected() != pos as u32 {
                row.output.set_selected(pos as u32);
            }
        }
        for extra in &row.extras {
            if let Some(device) = device_by_name(snap, &extra.sink_name) {
                extra.enable.set_label(Some(&device.description));
                extra.hint.set_text(&extra_hint(snap, stream, device));
            }
            let on = ui_ref
                .routes
                .iter()
                .any(|r| r.stream_index == stream.index && r.to_sink == extra.sink_name);
            if extra.enable.is_active() != on {
                extra.enable.set_active(on);
            }
            extra.item_box.remove_css_class("active-copy");
            if on {
                extra.item_box.add_css_class("active-copy");
            }
        }
    }
    drop(ui_ref);
    flags.building.set(false);
}

fn device_subtitle(snap: &Snapshot, device: &Device) -> String {
    let mark = if device.name == snap.default_sink { "★ Default · " } else { "" };
    format!("{mark}{} · {}", fmt_ms(device.latency_ms), device.sample)
}

fn extra_hint(snap: &Snapshot, stream: &Stream, dest: &Device) -> String {
    let main = device_by_index(snap, stream.sink);
    let main_ms = main.and_then(|d| d.latency_ms);
    let dest_ms = dest.latency_ms;
    match (main_ms, dest_ms) {
        (Some(main_ms), Some(dest_ms)) if dest_ms > main_ms + 8.0 => format!(
            "Target is {:.0} ms slower than primary. Consider setting it as primary for tight sync.",
            dest_ms - main_ms
        ),
        (Some(main_ms), Some(_)) => format!(
            "Auto syncs at {:.0} ms buffer to match primary output latency.",
            main_ms.round()
        ),
        _ => "Latency not reported. Adjust sync delay manually.".into(),
    }
}

fn volume_scale(volume: f64) -> Scale {
    let scale = Scale::with_range(Orientation::Horizontal, 0.0, 150.0, 1.0);
    scale.set_value(volume * 100.0);
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    scale.add_css_class("volume-scale");
    scale
}

fn wire_volume(
    scale: &Scale,
    flags: &Rc<Flags>,
    vol_label: Label,
    apply: impl Fn(f64) + 'static,
) {
    let apply: Rc<dyn Fn(f64)> = Rc::new(apply);
    let drag = GestureDrag::new();
    let flags_b = flags.clone();
    drag.connect_drag_begin(move |_, _, _| flags_b.dragging.set(true));
    let flags_e = flags.clone();
    let scale_e = scale.clone();
    let apply_e = apply.clone();
    let vol_lbl_e = vol_label.clone();
    drag.connect_drag_end(move |_, _, _| {
        flags_e.dragging.set(false);
        let val = scale_e.value();
        vol_lbl_e.set_text(&format!("{:.0}%", val));
        apply_e(val / 100.0);
    });
    scale.add_controller(drag);

    let flags_c = flags.clone();
    let vol_lbl_c = vol_label;
    scale.connect_value_changed(move |scale| {
        let val = scale.value();
        vol_lbl_c.set_text(&format!("{:.0}%", val));
        if flags_c.building.get() || flags_c.dragging.get() {
            return;
        }
        apply(val / 100.0);
    });
}

fn dropdown_from(labels: &[String]) -> DropDown {
    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    DropDown::new(Some(StringList::new(&refs)), None::<gtk4::Expression>)
}

fn port_dropdown(device: &Device) -> (DropDown, Vec<String>) {
    let labels: Vec<String> = device.ports.iter().map(|p| p.description.clone()).collect();
    let names: Vec<String> = device.ports.iter().map(|p| p.name.clone()).collect();
    let dd = dropdown_from(&labels);
    if let Some(pos) = names.iter().position(|n| n == &device.active_port) {
        dd.set_selected(pos as u32);
    }
    (dd, names)
}

fn shutdown(ui: &Rc<RefCell<Ui>>) {
    let mut ui_ref = ui.borrow_mut();
    for route in &mut ui_ref.routes {
        stop_child(&mut route.child);
    }
    ui_ref.routes.clear();
}

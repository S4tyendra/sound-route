mod audio;

use std::cell::{Cell, RefCell};
use std::process::Child;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    AlertDialog, Application, ApplicationWindow, Box, Button, CheckButton, DropDown, Frame,
    GestureDrag, Label, Orientation, Paned, PolicyType, Scale, ScrolledWindow, SpinButton,
    StringList, ToggleButton,
};

use audio::{
    align_delay_ms, device_by_index, device_by_name, fmt_ms, load_snapshot, move_stream, rescue,
    set_default_sink, set_device_mute, set_device_volume, set_sink_port, set_stream_mute,
    set_stream_volume, start_loopback, stop_child, stop_pid, Device, Loopback, Snapshot, Stream,
};

struct Route {
    stream_index: u32,
    to_sink: String,
    child: Child,
}

struct Flags {
    building: Cell<bool>,
    dragging: Cell<bool>,
    dialog: Cell<bool>,
}

struct DeviceRow {
    name: String,
    scale: Scale,
    mute: ToggleButton,
    port: DropDown,
    port_names: Vec<String>,
    latency: Label,
    state_label: Label,
    default_btn: Button,
}

struct ExtraRow {
    sink_name: String,
    enable: CheckButton,
    delay: SpinButton,
    whole: CheckButton,
    hint: Label,
}

struct StreamRow {
    index: u32,
    scale: Scale,
    mute: ToggleButton,
    output: DropDown,
    output_selected: Rc<Cell<u32>>,
    extras: Vec<ExtraRow>,
}

struct Ui {
    window: ApplicationWindow,
    devices_box: Box,
    streams_box: Box,
    info: Label,
    error: Label,
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
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Sound Route")
        .default_width(980)
        .default_height(680)
        .build();

    let root = Box::new(Orientation::Vertical, 8);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    root.set_margin_start(10);
    root.set_margin_end(10);

    let intro = Label::new(Some(
        "Volume and routing stay put until you change them. If a headset disconnects and a stream has nowhere to play, it is moved to the remaining output and copies aimed at the missing device are stopped.",
    ));
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    root.append(&intro);

    let devices_box = Box::new(Orientation::Vertical, 8);
    let streams_box = Box::new(Orientation::Vertical, 8);
    let paned = Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&scrolled(&devices_box)));
    paned.set_end_child(Some(&scrolled(&streams_box)));
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(true);
    paned.set_position(420);
    paned.set_vexpand(true);
    root.append(&paned);

    let info = Label::new(None);
    info.set_xalign(0.0);
    info.set_wrap(true);
    let error = Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    error.add_css_class("error");
    root.append(&info);
    root.append(&error);
    window.set_child(Some(&root));

    let ui = Rc::new(RefCell::new(Ui {
        window: window.clone(),
        devices_box,
        streams_box,
        info,
        error,
        device_rows: Vec::new(),
        stream_rows: Vec::new(),
        routes: Vec::new(),
        shape: String::new(),
        flags: Rc::new(Flags {
            building: Cell::new(false),
            dragging: Cell::new(false),
            dialog: Cell::new(false),
        }),
    }));

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

fn refresh(ui: &Rc<RefCell<Ui>>) {
    let flags = ui.borrow().flags.clone();
    if flags.dragging.get() || flags.dialog.get() {
        return;
    }
    let snap = match load_snapshot() {
        Ok(snap) => snap,
        Err(e) => {
            ui.borrow().error.set_text(&e);
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
                ui.borrow().error.set_text(&e);
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
    let counts = format!(
        "{} outputs, {} streams, {} copies started from this window.",
        snap.devices.len(),
        snap.streams.len(),
        managed
    );
    let text = if notes.is_empty() {
        counts
    } else {
        format!("{}\n{counts}", notes.join(" "))
    };
    ui.borrow().info.set_text(&text);
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

    let heading = Label::new(Some("Outputs"));
    heading.set_xalign(0.0);
    heading.add_css_class("heading");
    devices_box.append(&heading);

    let mut device_rows = Vec::new();
    for device in &snap.devices {
        device_rows.push(build_device_row(ui, snap, device, &devices_box));
    }

    let heading = Label::new(Some("Streams"));
    heading.set_xalign(0.0);
    heading.add_css_class("heading");
    streams_box.append(&heading);
    let note = Label::new(Some(
        "Main output is the direct path, so video stays locked to that device. Extra outputs are copies. Put the slower device (usually Bluetooth) on Main, then use Auto.",
    ));
    note.set_xalign(0.0);
    note.set_wrap(true);
    streams_box.append(&note);

    let mut stream_rows = Vec::new();
    if snap.streams.is_empty() {
        let empty = Label::new(Some("Nothing is playing."));
        empty.set_xalign(0.0);
        streams_box.append(&empty);
    }
    for stream in &snap.streams {
        stream_rows.push(build_stream_row(ui, snap, stream, &streams_box));
    }

    let tracked: Vec<u32> = ui.borrow().routes.iter().map(|r| r.child.id()).collect();
    let orphans: Vec<_> = snap
        .orphans
        .iter()
        .filter(|o| !tracked.contains(&o.pid))
        .cloned()
        .collect();
    if !orphans.is_empty() {
        let label = Label::new(Some("Copies left from an earlier Sound Route window"));
        label.set_xalign(0.0);
        streams_box.append(&label);
        for orphan in orphans {
            let row = Box::new(Orientation::Horizontal, 8);
            let text = Label::new(Some(&format!("{} (pid {})", orphan.label, orphan.pid)));
            text.set_xalign(0.0);
            text.set_hexpand(true);
            let stop = Button::with_label("Stop");
            let ui_stop = ui.clone();
            let pid = orphan.pid;
            stop.connect_clicked(move |_| match stop_pid(pid) {
                Ok(()) => ui_stop.borrow().error.set_text(""),
                Err(e) => ui_stop.borrow().error.set_text(&e),
            });
            row.append(&text);
            row.append(&stop);
            streams_box.append(&row);
        }
    }

    let mut ui_ref = ui.borrow_mut();
    ui_ref.device_rows = device_rows;
    ui_ref.stream_rows = stream_rows;
    flags.building.set(false);
}

fn build_device_row(ui: &Rc<RefCell<Ui>>, snap: &Snapshot, device: &Device, parent: &Box) -> DeviceRow {
    let frame = Frame::new(Some(&device.description));
    let box_ = Box::new(Orientation::Vertical, 4);
    box_.set_margin_top(8);
    box_.set_margin_bottom(8);
    box_.set_margin_start(8);
    box_.set_margin_end(8);

    let latency = Label::new(Some(&device_subtitle(snap, device)));
    latency.set_xalign(0.0);
    latency.set_wrap(true);
    box_.append(&latency);

    let state_label = Label::new(Some(&device.state));
    state_label.set_xalign(0.0);
    box_.append(&state_label);

    let scale = volume_scale(device.volume);
    let flags = ui.borrow().flags.clone();
    let name = device.name.clone();
    let ui_vol = ui.clone();
    wire_volume(&scale, &flags, move |value| {
        if let Err(e) = set_device_volume(&name, value) {
            ui_vol.borrow().error.set_text(&e);
        }
    });
    box_.append(&scale);

    let controls = Box::new(Orientation::Horizontal, 8);
    let mute = ToggleButton::with_label("Mute");
    mute.set_active(device.muted);
    let flags_m = flags.clone();
    let name = device.name.clone();
    let ui_m = ui.clone();
    mute.connect_toggled(move |btn| {
        if flags_m.building.get() {
            return;
        }
        if let Err(e) = set_device_mute(&name, btn.is_active()) {
            ui_m.borrow().error.set_text(&e);
        }
    });
    controls.append(&mute);

    let default_btn = Button::with_label(if device.name == snap.default_sink {
        "Default"
    } else {
        "Make default"
    });
    default_btn.set_sensitive(device.name != snap.default_sink);
    let name = device.name.clone();
    let desc = device.description.clone();
    let ui_d = ui.clone();
    default_btn.connect_clicked(move |_| {
        let ui_action = ui_d.clone();
        let name = name.clone();
        confirm(
            &ui_d,
            "Change the default output?",
            &format!("New playback will use {desc}. Streams that are already playing stay where they are."),
            "Make default",
            move || {
                if let Err(e) = set_default_sink(&name) {
                    ui_action.borrow().error.set_text(&e);
                } else {
                    ui_action.borrow().error.set_text("");
                    ui_action.borrow_mut().shape.clear();
                }
            },
        );
    });
    controls.append(&default_btn);
    box_.append(&controls);

    let (port, port_names) = port_dropdown(device);
    if port_names.len() > 1 {
        let flags_p = flags.clone();
        let name = device.name.clone();
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
            flags_p.building.set(true);
            dd.set_selected(old);
            flags_p.building.set(false);
            let ui_action = ui_p.clone();
            let port_name = port_name.clone();
            let name = name.clone();
            let previous = previous.clone();
            let dd = dd.clone();
            let flags_p = flags_p.clone();
            confirm(
                &ui_p,
                "Change the output port?",
                "This switches the jack or speaker on that device immediately.",
                "Switch",
                move || match set_sink_port(&name, &port_name) {
                    Ok(()) => {
                        previous.set(new);
                        flags_p.building.set(true);
                        dd.set_selected(new);
                        flags_p.building.set(false);
                        ui_action.borrow().error.set_text("");
                    }
                    Err(e) => ui_action.borrow().error.set_text(&e),
                },
            );
        });
        box_.append(&port);
    }

    frame.set_child(Some(&box_));
    parent.append(&frame);
    DeviceRow {
        name: device.name.clone(),
        scale,
        mute,
        port,
        port_names,
        latency,
        state_label,
        default_btn,
    }
}

fn build_stream_row(ui: &Rc<RefCell<Ui>>, snap: &Snapshot, stream: &Stream, parent: &Box) -> StreamRow {
    let frame = Frame::new(Some(&stream.title));
    let box_ = Box::new(Orientation::Vertical, 4);
    box_.set_margin_top(8);
    box_.set_margin_bottom(8);
    box_.set_margin_start(8);
    box_.set_margin_end(8);

    if !stream.detail.is_empty() {
        let detail = Label::new(Some(&stream.detail));
        detail.set_xalign(0.0);
        detail.set_wrap(true);
        box_.append(&detail);
    }

    let scale = volume_scale(stream.volume);
    let flags = ui.borrow().flags.clone();
    let index = stream.index;
    let ui_v = ui.clone();
    wire_volume(&scale, &flags, move |value| {
        if let Err(e) = set_stream_volume(index, value) {
            ui_v.borrow().error.set_text(&e);
        }
    });
    box_.append(&scale);

    let mute = ToggleButton::with_label("Mute");
    mute.set_active(stream.muted);
    let flags_m = flags.clone();
    let ui_m = ui.clone();
    mute.connect_toggled(move |btn| {
        if flags_m.building.get() {
            return;
        }
        if let Err(e) = set_stream_mute(index, btn.is_active()) {
            ui_m.borrow().error.set_text(&e);
        }
    });
    box_.append(&mute);

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
        flags_o.building.set(true);
        dd.set_selected(old);
        flags_o.building.set(false);
        let ui_action = ui_o.clone();
        let previous = previous.clone();
        let dd = dd.clone();
        let flags_o = flags_o.clone();
        confirm(
            &ui_o,
            "Move this stream?",
            "Playback switches to that output immediately. Extra copies keep following this stream.",
            "Move",
            move || match move_stream(stream_index, &sink_name) {
                Ok(()) => {
                    previous.set(new);
                    flags_o.building.set(true);
                    dd.set_selected(new);
                    flags_o.building.set(false);
                    ui_action.borrow().error.set_text("");
                    ui_action.borrow_mut().shape.clear();
                    restart_whole_output_routes(&ui_action, stream_index, &sink_name);
                }
                Err(e) => ui_action.borrow().error.set_text(&e),
            },
        );
    });
    let main_row = Box::new(Orientation::Horizontal, 8);
    let main_label = Label::new(Some("Main"));
    main_row.append(&main_label);
    output.set_hexpand(true);
    main_row.append(&output);
    box_.append(&main_row);

    let mut extras = Vec::new();
    if snap.devices.len() > 1 {
        let also = Label::new(Some("Also play on"));
        also.set_xalign(0.0);
        box_.append(&also);
    }
    for device in &snap.devices {
        if device.index == stream.sink {
            continue;
        }
        extras.push(build_extra(ui, snap, stream, device, &box_));
    }

    let align = Button::with_label("Align checked copies");
    let ui_a = ui.clone();
    let stream_index = stream.index;
    align.connect_clicked(move |_| align_checked(&ui_a, stream_index));
    box_.append(&align);

    frame.set_child(Some(&box_));
    parent.append(&frame);
    StreamRow {
        index: stream.index,
        scale,
        mute,
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
    let enable = CheckButton::with_label(&format!("{} · {}", device.description, fmt_ms(device.latency_ms)));
    let already = ui
        .borrow()
        .routes
        .iter()
        .any(|r| r.stream_index == stream.index && r.to_sink == device.name);
    enable.set_active(already);

    let delay = SpinButton::with_range(0.0, 2000.0, 1.0);
    delay.set_digits(0);
    let main_latency = device_by_index(snap, stream.sink).and_then(|d| d.latency_ms);
    delay.set_value(align_delay_ms(main_latency).round());

    let whole = CheckButton::with_label("Copy everything on the main output");
    whole.set_tooltip_text(Some(
        "Off: only this stream. On: every sound already going to the main output.",
    ));

    let hint = Label::new(Some(&extra_hint(snap, stream, device)));
    hint.set_xalign(0.0);
    hint.set_wrap(true);

    let flags = ui.borrow().flags.clone();
    let ui_e = ui.clone();
    let stream_index = stream.index;
    let to_sink = device.name.clone();
    let delay_for_toggle = delay.clone();
    let whole_for_toggle = whole.clone();
    enable.connect_toggled(move |btn| {
        if flags.building.get() {
            return;
        }
        if !btn.is_active() {
            stop_route(&ui_e, stream_index, &to_sink);
            return;
        }
        flags.building.set(true);
        btn.set_active(false);
        flags.building.set(false);
        let ui_action = ui_e.clone();
        let btn = btn.clone();
        let flags = flags.clone();
        let to_sink = to_sink.clone();
        let delay_ms = delay_for_toggle.value();
        let whole_output = whole_for_toggle.is_active();
        confirm(
            &ui_e,
            "Copy this audio?",
            "A second copy starts on that device. The main output is left as it is.",
            "Copy",
            move || {
                let snap = match load_snapshot() {
                    Ok(s) => s,
                    Err(e) => {
                        ui_action.borrow().error.set_text(&e);
                        return;
                    }
                };
                let Some(stream) = snap.streams.iter().find(|s| s.index == stream_index) else {
                    ui_action.borrow().error.set_text("That stream is no longer playing.");
                    return;
                };
                let from_sink = device_by_index(&snap, stream.sink)
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                let serial = stream.serial.clone();
                match spawn_route(
                    &ui_action,
                    stream_index,
                    &serial,
                    &from_sink,
                    &to_sink,
                    whole_output,
                    delay_ms,
                ) {
                    Ok(()) => {
                        flags.building.set(true);
                        btn.set_active(true);
                        flags.building.set(false);
                        ui_action.borrow().error.set_text("");
                    }
                    Err(e) => ui_action.borrow().error.set_text(&e),
                }
            },
        );
    });

    let row = Box::new(Orientation::Horizontal, 8);
    delay.set_tooltip_text(Some(
        "How long this copy is held so it lines up with the main output. Auto uses the main output's measured latency.",
    ));
    let apply = Button::with_label("Apply");
    let auto = Button::with_label("Auto");
    let ui_apply = ui.clone();
    let to_sink_apply = device.name.clone();
    let stream_index_apply = stream.index;
    let delay_apply = delay.clone();
    let whole_apply = whole.clone();
    apply.connect_clicked(move |_| {
        apply_delay(
            &ui_apply,
            stream_index_apply,
            &to_sink_apply,
            whole_apply.is_active(),
            delay_apply.value(),
        );
    });
    let ui_auto = ui.clone();
    let to_sink_auto = device.name.clone();
    let delay_auto = delay.clone();
    let whole_auto = whole.clone();
    let stream_index_auto = stream.index;
    auto.connect_clicked(move |_| {
        let snap = match load_snapshot() {
            Ok(s) => s,
            Err(e) => {
                ui_auto.borrow().error.set_text(&e);
                return;
            }
        };
        let Some(stream) = snap.streams.iter().find(|s| s.index == stream_index_auto) else {
            ui_auto.borrow().error.set_text("That stream is no longer playing.");
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
            ui_auto.borrow().error.set_text(&format!(
                "Sync delay set to {:.0} ms. Turn the copy on to use it.",
                ms.round()
            ));
        }
    });

    row.append(&delay);
    row.append(&apply);
    row.append(&auto);
    parent.append(&enable);
    parent.append(&row);
    parent.append(&whole);
    parent.append(&hint);

    ExtraRow {
        sink_name: device.name.clone(),
        enable,
        delay,
        whole,
        hint,
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
        return Err("This stream has no PipeWire serial. Turn on 'Copy everything on the main output'.".into());
    }
    if from_sink.is_empty() {
        return Err("The main output is not available.".into());
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
        ui.borrow()
            .error
            .set_text("Turn the copy on first. Apply then restarts it with this delay.");
        return;
    }
    let snap = match load_snapshot() {
        Ok(s) => s,
        Err(e) => {
            ui.borrow().error.set_text(&e);
            return;
        }
    };
    let Some(stream) = snap.streams.iter().find(|s| s.index == stream_index) else {
        ui.borrow().error.set_text("That stream is no longer playing.");
        return;
    };
    let from_sink = device_by_index(&snap, stream.sink)
        .map(|d| d.name.clone())
        .unwrap_or_default();
    let serial = stream.serial.clone();
    match spawn_route(ui, stream_index, &serial, &from_sink, to_sink, whole_output, delay_ms) {
        Ok(()) => ui.borrow().error.set_text(""),
        Err(e) => ui.borrow().error.set_text(&e),
    }
}

fn align_checked(ui: &Rc<RefCell<Ui>>, stream_index: u32) {
    let snap = match load_snapshot() {
        Ok(s) => s,
        Err(e) => {
            ui.borrow().error.set_text(&e);
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
        ui.borrow().error.set_text("No extra output is checked.");
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
            ui.borrow().error.set_text(&e);
            return;
        }
    }
    ui.borrow().error.set_text("");
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
            ui.borrow().error.set_text(&e);
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
        ui.borrow().error.set_text(
            "A copy stopped. PipeWire would not link that stream. Try 'Copy everything on the main output'.",
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
        }
        row.mute.set_active(device.muted);
        row.latency.set_text(&device_subtitle(snap, device));
        row.state_label.set_text(&device.state);
        let is_default = device.name == snap.default_sink;
        row.default_btn.set_label(if is_default { "Default" } else { "Make default" });
        row.default_btn.set_sensitive(!is_default);
        if let Some(pos) = row.port_names.iter().position(|n| n == &device.active_port) {
            row.port.set_selected(pos as u32);
        }
    }
    for row in &ui_ref.stream_rows {
        let Some(stream) = snap.streams.iter().find(|s| s.index == row.index) else {
            continue;
        };
        if !flags.dragging.get() {
            row.scale.set_value(stream.volume * 100.0);
        }
        row.mute.set_active(stream.muted);
        if let Some(pos) = snap.devices.iter().position(|d| d.index == stream.sink) {
            row.output_selected.set(pos as u32);
            if row.output.selected() != pos as u32 {
                row.output.set_selected(pos as u32);
            }
        }
        for extra in &row.extras {
            if let Some(device) = device_by_name(snap, &extra.sink_name) {
                extra.enable.set_label(Some(&format!(
                    "{} · {}",
                    device.description,
                    fmt_ms(device.latency_ms)
                )));
                extra.hint.set_text(&extra_hint(snap, stream, device));
            }
            let on = ui_ref
                .routes
                .iter()
                .any(|r| r.stream_index == stream.index && r.to_sink == extra.sink_name);
            if extra.enable.is_active() != on {
                extra.enable.set_active(on);
            }
        }
    }
    drop(ui_ref);
    flags.building.set(false);
}

fn device_subtitle(snap: &Snapshot, device: &Device) -> String {
    let mark = if device.name == snap.default_sink { "default · " } else { "" };
    format!("{mark}{} · {}", fmt_ms(device.latency_ms), device.sample)
}

fn extra_hint(snap: &Snapshot, stream: &Stream, dest: &Device) -> String {
    let main = device_by_index(snap, stream.sink);
    let main_ms = main.and_then(|d| d.latency_ms);
    let dest_ms = dest.latency_ms;
    match (main_ms, dest_ms) {
        (Some(main_ms), Some(dest_ms)) if dest_ms > main_ms + 8.0 => format!(
            "This device is about {:.0} ms slower than the main output, so a copy here stays late. Make it the main output to line them up.",
            dest_ms - main_ms
        ),
        (Some(main_ms), Some(_)) => format!(
            "Auto holds this copy for {:.0} ms, which is the main output's latency.",
            main_ms.round()
        ),
        _ => "Latency is not reported yet. Set the sync delay yourself, then Apply.".into(),
    }
}

fn volume_scale(volume: f64) -> Scale {
    let scale = Scale::with_range(Orientation::Horizontal, 0.0, 150.0, 1.0);
    scale.set_value(volume * 100.0);
    scale.set_draw_value(true);
    scale.set_hexpand(true);
    scale
}

fn wire_volume(scale: &Scale, flags: &Rc<Flags>, apply: impl Fn(f64) + 'static) {
    let apply: Rc<dyn Fn(f64)> = Rc::new(apply);
    let drag = GestureDrag::new();
    let flags_b = flags.clone();
    drag.connect_drag_begin(move |_, _, _| flags_b.dragging.set(true));
    let flags_e = flags.clone();
    let scale_e = scale.clone();
    let apply_e = apply.clone();
    drag.connect_drag_end(move |_, _, _| {
        flags_e.dragging.set(false);
        apply_e(scale_e.value() / 100.0);
    });
    scale.add_controller(drag);
    let flags_c = flags.clone();
    scale.connect_value_changed(move |scale| {
        if flags_c.building.get() || flags_c.dragging.get() {
            return;
        }
        apply(scale.value() / 100.0);
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

fn confirm(ui: &Rc<RefCell<Ui>>, title: &str, detail: &str, accept: &str, then: impl FnOnce() + 'static) {
    let flags = ui.borrow().flags.clone();
    if flags.dialog.get() {
        return;
    }
    flags.dialog.set(true);
    let dialog = AlertDialog::builder()
        .message(title)
        .detail(detail)
        .buttons(["Cancel", accept])
        .cancel_button(0)
        .default_button(1)
        .modal(true)
        .build();
    let window = ui.borrow().window.clone();
    dialog.choose(Some(&window), None::<&gtk4::gio::Cancellable>, move |res| {
        flags.dialog.set(false);
        if res == Ok(1) {
            then();
        }
    });
}

fn shutdown(ui: &Rc<RefCell<Ui>>) {
    let mut ui_ref = ui.borrow_mut();
    for route in &mut ui_ref.routes {
        stop_child(&mut route.child);
    }
    ui_ref.routes.clear();
}

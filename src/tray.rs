use std::sync::mpsc::Sender;

use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;

pub enum TrayCommand {
    Show,
    Quit,
}

pub struct TrayIcon {
    handle: ksni::blocking::Handle<SoundTray>,
}

impl TrayIcon {
    pub fn start(cmd: Sender<TrayCommand>) -> Result<Self, String> {
        let tray = SoundTray {
            cmd,
            icon: load_icon(),
        };
        let handle = tray.spawn().map_err(|err| err.to_string())?;
        Ok(Self { handle })
    }

    pub fn stop(self) {
        self.handle.shutdown().wait();
    }
}

struct SoundTray {
    cmd: Sender<TrayCommand>,
    icon: Vec<ksni::Icon>,
}

impl ksni::Tray for SoundTray {
    fn id(&self) -> String {
        "sound-route".into()
    }

    fn title(&self) -> String {
        "Sound Route".into()
    }

    fn icon_name(&self) -> String {
        "sound-route".into()
    }

    fn icon_theme_path(&self) -> String {
        "/usr/share/icons/hicolor".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: "sound-route".into(),
            icon_pixmap: self.icon.clone(),
            title: "Sound Route".into(),
            description: "Routes are still running. Click to show the window.".into(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.cmd.send(TrayCommand::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.cmd.send(TrayCommand::Show);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit and stop routes".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.cmd.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn load_icon() -> Vec<ksni::Icon> {
    let paths = [
        "/usr/share/pixmaps/sound-route.png",
        "/usr/share/icons/hicolor/256x256/apps/sound-route.png",
    ];
    for path in paths {
        if let Some(icon) = png_icon(path, 64) {
            return vec![icon];
        }
    }
    Vec::new()
}

fn png_icon(path: &str, size: i32) -> Option<ksni::Icon> {
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true).ok()?;
    let width = pixbuf.width();
    let height = pixbuf.height();
    if width <= 0 || height <= 0 {
        return None;
    }
    let stride = pixbuf.rowstride() as usize;
    let channels = pixbuf.n_channels() as usize;
    if channels < 3 {
        return None;
    }
    let bytes = pixbuf.pixel_bytes()?;
    let src = bytes.as_ref();
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        let row = src.get(y * stride..)?;
        for x in 0..width as usize {
            let i = x * channels;
            let r = *row.get(i)?;
            let g = *row.get(i + 1)?;
            let b = *row.get(i + 2)?;
            let a = if channels >= 4 { *row.get(i + 3)? } else { 255 };
            data.extend_from_slice(&[a, r, g, b]);
        }
    }
    Some(ksni::Icon { width, height, data })
}

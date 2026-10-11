# Sound Route

PipeWire audio router.

Per output: volume, mute, make it the default, switch the port (speakers, headphones, whatever the card exposes).

Per app: volume, mute, and a primary output. That primary path is the real one, so video stays locked to it.

Fan-out copies the same audio to other devices. Set a delay in ms, or hit Auto / Auto-Sync All and it uses the primary output's measured latency so a Bluetooth headset and the speakers land together. "Mirror master mix" copies everything already playing on the primary output, not just that one app.

If a device disappears, a stream left with nowhere to go gets moved to an output that is still there. Copies aimed at the dead device get stopped.

You need PipeWire. No PipeWire, no app. You also need Rust, GTK4, and pkg-config.

```
git clone https://github.com/S4tyendra/sound-route
cd sound-route
./deploy.sh
```

sudo asks for a password. That installs the binary, the icon, and a menu launcher.

Run `sound-route`, or open Sound Route from the menu.

Tray is on. Closing the window does not kill your routes. Quit from the tray when you want them gone.

Build it on your machine. No binary here.

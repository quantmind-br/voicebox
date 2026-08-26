# Voicebox on Arch Linux + Hyprland (Wayland)

Everything in the app works on this stack, including dictation auto-paste and
the floating dictation pill. Getting there needs a few things Wayland cannot
provide on its own, which this page explains rather than just lists — the
required packages are only obvious once you know what they stand in for.

## Why Wayland needs extra pieces

Four capabilities that macOS and Windows each expose through one system call
are, on Wayland, compositor privileges that no client can ask for:

| What auto-paste needs | Why Wayland says no | What Voicebox uses instead |
|---|---|---|
| Read the focused window | No protocol exposes other clients' focus | Hyprland's IPC socket |
| Raise a window | Clients cannot raise each other | Hyprland's IPC socket |
| Own the clipboard | Only the focused client may write it | `wlr-`/`ext-data-control` |
| Synthesise Ctrl+V | No XTEST equivalent exists | `wtype`, else `/dev/uinput` |

Two consequences follow. First, Hyprland (or any wlroots compositor) is what
makes focus restore possible — on GNOME, which implements neither
data-control nor an equivalent IPC, auto-paste cannot work at all. Second, the
keystroke needs *one* of `wtype` or `/dev/uinput` access, and which one you
pick has a real difference attached (see below).

## Install

```sh
# Required
sudo pacman -S --needed webkit2gtk-4.1 gtk3 alsa-lib libappindicator-gtk3 \
                        librsvg openssl

# Auto-paste: wtype is strongly preferred, see "wtype vs uinput" below
sudo pacman -S --needed wtype

# GPU acceleration — pick the one matching your hardware
sudo pacman -S --needed cuda            # NVIDIA
sudo pacman -S --needed rocm-hip-runtime # AMD

# System-audio capture reads a PipeWire/PulseAudio monitor source
sudo pacman -S --needed libpulse
```

### wtype vs uinput

Auto-paste needs to synthesise Ctrl+V. Both mechanisms work; they fail
differently.

**`wtype` (recommended)** uploads its own xkb keymap to the compositor before
sending the keystroke, so the key that arrives is genuinely `v` no matter what
layout you use. It needs no permissions.

**`/dev/uinput`** injects below the compositor, which makes it more universal —
but it emits *scancodes*, and a scancode is a physical key position. On a
Dvorak layout `KEY_V` is where `.` lives, so the injected accelerator becomes
Ctrl+`.` and pastes nothing. It also needs write access to a device that can
type into any application on your seat:

```sh
sudo install -Dm644 packaging/arch/99-voicebox-uinput.rules \
  /usr/lib/udev/rules.d/99-voicebox-uinput.rules
sudo usermod -aG input "$USER"   # log out and back in
```

Adding yourself to `input` also grants read access to every
`/dev/input/event*`, which is enough to keylog your own session. If that is not
a trade you want, install `wtype` and skip this entirely.

> Voicebox already needs `input` group membership for its **global hotkey**,
> which reads evdev directly — that part works on Wayland precisely because it
> bypasses the compositor. So on most setups the group is already in place and
> only the udev rule is new.

## Run from source

```sh
just setup          # Python venv (~9 GB: PyTorch + TTS runtimes) and JS deps
bun run dev:server  # backend on :17493, in one terminal
bun run dev         # desktop app, in another
```

`just setup-python` detects your GPU and installs the matching PyTorch build —
CUDA when `/proc/driver/nvidia/version` exists, ROCm when `/dev/kfd` does, CPU
otherwise. NVIDIA is checked first, which is what you want on a machine that
has both a discrete NVIDIA card and an AMD iGPU.

The venv is built with Python 3.12 when available. This is not incidental:
the ML stack pins `numpy<2` and `numba<0.61`, neither of which publishes
wheels for 3.13+, so a 3.14 system interpreter will fail to build them.

## Build a package

```sh
cd packaging/arch && makepkg -si
```

Expect a multi-gigabyte package and roughly 25 GB of scratch space. The Python
backend is frozen with PyInstaller and carries PyTorch plus every TTS runtime;
the macOS and Windows builds have the same shape. Running from source avoids
the duplication if you do not need an installed copy.

## The dictation pill

The floating pill is a Wayland toplevel, and a Wayland toplevel cannot place
itself, stay on top, pin across workspaces, or refuse focus — those are all
X11-era GTK hints that get silently dropped. Voicebox therefore registers
Hyprland window rules for its own title at startup, and places the pill through
the compositor. Nothing to configure; it works on a stock Hyprland.

If you want to override the placement, the pill's title is exactly
`Voicebox Dictate`:

```
windowrule = move 100 100, title:^(Voicebox Dictate)$
```

Your rule is applied after Voicebox's, so it wins.

The one rule worth understanding is `no_focus`. Hyprland focuses newly mapped
windows by default, so without it the pill would steal the keyboard from
whatever you are dictating into the instant it appears.

## NVIDIA: blank window

WebKitGTK's DMABUF renderer and the NVIDIA driver disagree on some
driver/compositor pairings, and the failure mode is a window that maps and
paints nothing. Voicebox detects NVIDIA-on-Wayland at startup and disables that
renderer, trading some compositing performance for a UI that is actually
visible.

If your setup renders fine with it on, opt back into the fast path:

```sh
VOICEBOX_WEBKIT_DMABUF=1 voicebox
```

An explicit `WEBKIT_DISABLE_DMABUF_RENDERER` in your environment always wins
over both.

## Troubleshooting

**Auto-paste does nothing.** Settings → Captures shows a readiness row naming
the missing piece. Usually `wtype` is not installed and `/dev/uinput` is not
writable.

**Paste lands in a terminal as a control character.** Terminals bind
Ctrl+Shift+V, not Ctrl+V. Voicebox detects the common emulators by app-id and
switches automatically; if yours is missed, the app-id from `hyprctl
activewindow` is what needs adding to `TERMINAL_CLASS_MARKERS` in
`tauri/src-tauri/src/linux/paste.rs`.

**Global hotkey does nothing.** The chord engine reads evdev directly, so it
needs `input` group membership — `id -nG | grep input`. Log out and back in
after `usermod`.

**System-audio capture finds no device.** It looks for a PipeWire/PulseAudio
monitor source via `pactl list short sources | grep monitor`. If that is empty,
no output is being monitored.

**The pill appears tiled, or steals focus.** The window rules did not register.
Check `hyprctl version` — Voicebox supports both the pre-0.56 `keyword
windowrulev2` dialect and the Lua `hl.window_rule` API that replaced it, and
falls back to the older one when the probe is inconclusive.

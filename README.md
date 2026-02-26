# waygaps
Configurable hot corners amd hot edges for wayland. Lets you add more functionality to your gaps.

## Installation
*Waygaps currently only supports x86_64 linux. It might be possible to build on other architectures or (unix based) operating systems, but I haven't tried.*

### NixOS
Add to flake inputs:
```nix
waygaps = {
    url = "github:Ind-E/waygaps";
    inputs.nixpkgs.follows = "nixpkgs";
};
```
then add to `environment.systemPackages` or similar:
```nix
inputs.waygaps.packages.${stdenv.hostPlatform.system}.default
```

### Build From Source
You'll need a nightly version of the rust toolchain with the `rust-src` component, and the following dependencies:
```
wayland-protocols
wayland-scanner
```


Then, it _should_ be as simple as
```sh
git clone git@github.com:Ind-E/waygaps.git
cd waygaps
cargo install --path . --locked
```
This might not work because of some of the bespoke (unstable) configuration options employed.
In that case, open an issue.


## Quickstart
Copy the [example config](./example-config.toml) to `~/.config/waygaps/config.toml`.
Run `waygaps -p` while configuring to preview where the hot corners are, then setup a way
to run `waygaps` at startup.

See below for more details


## Configuration
```toml
#:schema https://raw.githubusercontent.com/Ind-E/waygaps/refs/heads/main/schema.json

# name of the hot corner - can be anything
[hot-corner-1]

# use "top-right", "top-left", "bottom-right", or "bottom-left" for corners
# use "top", "bottom", "left", or "right" for edges
# default: "top-left"
anchor = "top-right"

# for corners, affects width and height
# for edges, affects length from edge of screen
# default: 10
size = 50

# searches for any outputs that contain this in their name or description
# you can use a tool like wlr-randr to check names/descriptions
# defaults to all outputs if not included
output = "eDP-1"

# only affects edges
# useful to avoid overlapping edges and corners
# default: 0
margin = 10

# affects how much you have to move the mouse beyond the edge of the
# screen to trigger the 'edge' action (see below)
# default: 25
activation-force = 20

# which layer to draw the region on
# default: overlay
layer = "top"

# whether or not to ignore the exclusive zone of other layer shell surfaces
# default: true
ignore-exclusive-zone = false

# color used to preview this region when the -p flag is passed
preview-color = { r = 128, g = 16, b = 16, a = 25 }

# list of actions and corresponding command to run (with sh)
commands = [
  # triggers when pointer enters the region
  ["enter", "notify-send enter"],

  # triggers when pointer leaves the region
  ["leave", "notify-send leave"],

  # triggers when pointer is pushed up against the screen edge/corner
  # configure `activation-force` above to make easier/harder to trigger
  ["edge", "notify-send edge"],

  # scroll triggers
  ["scroll-up", "notify-send up"],
  ["scroll-down", "notify-send down"],
  ["scroll-left", "notify-send left"],
  ["scroll-right", "notify-send right"],

  # mouse button triggers
  ["mouse-left", "notify-send 'left click'"],
  ["mouse-right", "notify-send 'right click'"],
  ["mouse-middle", "notify-send 'middle click'"],

  # for other mouse buttons, use their input event codes
  # you can use a tool like wev to find them
  ["mouse-274", "notify-send 'back button'"],
]

# add as many hot corners as you want
[hot-corner-2]
# etc
```
[wev](https://github.com/jwrdegoede/wev) [wlr-randr](https://gitlab.freedesktop.org/emersion/wlr-randr)


## Features
- Preview hot corners with the `--preview` (short: `-p`) flag
- Specify a config file with the `--config` (short: `-c`) flag


## Caveats
- Running waygaps requires a compositor that implements the wlr layer shell
  protocol protocol. You can check
  [here](https://wayland.app/protocols/wlr-layer-shell-unstable-v1#compositor-support)
  if your compositor supports it (most of them do)
- Pointer events on the configured regions won't be passed to clients below;
  they will be swallowed. This is a limitation in how wayland works, presumably to prevent a malicious app from
  tracking all your inputs with a transparent layer over the whole screen
- Mouse actions won't be triggered by touch events (I could theoretically add support for this, I just don't see the use case)
- Multiple monitors _probably_ works, but I don't have an extra monitor test with


## Performance
As part of creating waygaps, I challenged myself to make something extremely fast
and lightweight, even if it came at the cost of usability. This does come with some
downsides - it's not very portable and there's no way to enable extra logging in release builds.

I think it was worth it, though - On my laptop, waygaps uses about 500 Kb of memory,
and idles at 0.0% cpu usage.
```sh
❯ ps -C waygaps -o cmd,pcpu,rss,pss,sz,vsize
CMD        %CPU   RSS   PSS    SZ    VSZ
waygaps     0.0   536   309   168    672
```


## Inspiration
I was using [waycorner](https://github.com/AndreasBackx/waycorner) for a bit, but I ran into a couple pain points:
I couldn't bind mouse clicks/scrolls, and the way it used a timeout to trigger commands didn't feel
quite right. That gave me the idea to create a hot corners tool that does let you bind mouse clicks and has
pressure barriers to trigger actions instead of a timeout. That idea floated around in the back of my mind until I
happened across this [blog post](https://www.lgfae.com/posts/2025-11-21-SettingAWallpaperWithLessThan250KB.html)
by LGFaé, which inspired me to use their low overhead [waybackend](https://codeberg.org/LGFae/waybackend)
library to create waygaps.

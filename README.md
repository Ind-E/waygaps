# waygaps
Configurable hot corners and hot edges for wayland

## Installation

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
You'll need a nightly version of the rust toolchain, and the following dependencies:
```
wayland-protocols
wayland-scanner
```


Then, it should be as simple as building any other cargo project
```sh
git clone git@github.com:Ind-E/waygaps.git
cd waygaps
cargo install --path . --locked
```


## Quickstart
- Copy the [example config](./example-config.toml) to `~/.config/waygaps/config.toml`
  - Or, run `waygaps --print-example-config > ~/.config/waygaps/config.toml`
- Run `waygaps -p` while configuring to preview where the hot corners are
- Once satisfied, configure waygaps to run at startup

See below for an annotated configuration


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
start-margin = 10
end-margin = 10

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
  ["mouse-275", "notify-send 'back button'"],
]

# add as many hot corners as you want
[hot-corner-2]
# etc
```
tools mentioned above: [wev](https://github.com/jwrdegoede/wev), [wlr-randr](https://gitlab.freedesktop.org/emersion/wlr-randr)


## Features
- Preview hot corners with the `-p | --preview` flag
- Specify a config file with the `-c | --config` flag


## Caveats
- Running waygaps requires a compositor that implements the wlr layer shell
  protocol protocol. You can check
  [here](https://wayland.app/protocols/wlr-layer-shell-unstable-v1#compositor-support)
  if your compositor supports it (most of them do)
- Pointer events on the configured regions won't be passed to clients below;
  they will be swallowed. This is a limitation in how wayland works, in order to prevent a malicious app from
  tracking all your inputs with a transparent layer over the whole screen
- Fullscreen windows will prevent all but overlay regions from working (overlay regions will prevent direct scanout, however)
- Mouse actions won't be triggered by touch events (I could theoretically add support for this, I just don't see the use case)
- Multiple monitors _probably_ works, but I don't have an extra monitor test with


## Performance
When creating waygaps, I aimed for something extremely fast and lightweight, even if it's a bit
rough around the edges.

I'd like to think I achieved that goal - On my laptop, waygaps uses about 500 Kb of memory,
and idles at 0.0% cpu usage, spiking up to 0.1% under heavy use.
```sh
❯ ps -C waygaps -o cmd,pcpu,rss,pss,sz,vsize
CMD        %CPU   RSS   PSS    SZ    VSZ
waygaps     0.0   536   309   168    672
```


## Inspiration
I was using [waycorner](https://github.com/AndreasBackx/waycorner) for a bit, but I ran into a couple pain points:
I couldn't bind mouse clicks/scrolls, and the way it used a timeout to trigger commands didn't feel
quite right to me. That gave me the idea to create a hot corners tool that does let you bind mouse clicks and has
pressure barriers to trigger actions instead of a timeout. That idea floated around in the back of my mind until I
happened across this [blog post](https://www.lgfae.com/posts/2025-11-21-SettingAWallpaperWithLessThan250KB.html)
by LGFaé, which inspired me to use their low overhead [waybackend](https://codeberg.org/LGFae/waybackend)
library to create waygaps.

# Nodo

<img src="crates/app/assets/logo/nodo-icon-512.png" width="128" alt="Nodo icon">

Map and track the devices on your networks — your LAN, your Tailscale tailnet, your AWS VPCs and
Client VPN, your WireGuard tunnels — as a living graph. Who is there, what it is, whether it is up,
and what you know about it. *Nodo* is Spanish for *node*.

Native desktop app in Rust with [GPUI](https://www.gpui.rs/) and
[gpui-component](https://github.com/longbridge/gpui-component). macOS, Windows and Linux. UI in
Spanish, English, Portuguese and French. Plus `nodo`, a headless command line on the same engine,
for scripts and AI agents.

## Features

- **Graph**: the gateway / router in the centre, subnets and networks as hubs, every device around
  them. Layers for **LAN**, **Tailscale**, **AWS** and **WireGuard**, each its own colour and each
  toggleable; a device on several networks (your laptop on the LAN and the tailnet) gets a dashed
  edge to its other networks. Offline devices are greyed, down ones red. Three layouts: **radial**,
  **by subnet** (a box per subnet) and **by network** (a column per layer). Drag or scroll to pan,
  ⌘ + scroll, pinch or ⌘± to zoom, ⌘0 fits the map. **List** mode (⌘L) sorts by status, name,
  type, IP, network or last seen. **Search** (⌘F) by name, IP, MAC, vendor, model, owner, tag or
  note: everything else fades; Enter jumps to the first match. A side panel shows the selected
  device.
- **Discovery, passive first**:
  - the neighbour table (`arp -an` / `ndp -an` on macOS, `/proc/net/arp` and `ip neigh` on Linux,
    `arp -a` on Windows), with the MAC vendor from an embedded, compressed copy of the IEEE OUI
    registry (offline);
  - mDNS / Bonjour (`_http`, `_ssh`, `_smb`, `_ipp`, `_airplay`, `_googlecast`, `_hap`,
    `_homeassistant`, `_device-info` and 20 more): names, models, services;
  - SSDP / UPnP: one `M-SEARCH`, then each device description (friendly name, maker, model, web
    page) fetched from private addresses only.

  Passive discovery runs when a map opens and every 5 minutes while it is open (can be turned off).
- **Active scan, only when you ask** (*Scan subnet…*): one ping and a TCP connection attempt to
  22, 80, 443, 445, 3389, 5900, 8080, 9100 and 554 for each address of the current /24, 32 hosts at
  a time and at most 400 attempts per second; open ports hint the device type. Private ranges only,
  with a clear *only scan networks you own or administer* warning before it starts.
- **Tailscale**: `tailscale status --json` from the CLI on your PATH or inside
  `/Applications/Tailscale.app` (or `~/Applications`): every node with its name, addresses, OS,
  online state, exit node, tags, owner (the tailnet user's name; group the list by owner) and
  whether the connection is direct or relayed. When Tailscale is installed it runs with every
  refresh (on open, ⌘R and every few minutes); the side panel's Sources section shows
  "Tailscale: 12 devices · updated 10:42" or what went wrong.
- **AWS**, read-only, through **your own AWS CLI** and profile (SSO works): `aws ec2
  describe-vpcs / describe-subnets / describe-instances / describe-security-groups /
  describe-client-vpn-endpoints --output json`, for the regions you choose. VPCs and their subnets
  become networks, instances and Client VPN endpoints devices, with type, state, AZ, public IP and
  security groups. Nodo never sees, asks for or stores credentials; an expired SSO session is
  explained (`aws sso login --profile …`).
- **WireGuard**: `wg show all dump` — interfaces become networks, peers devices (allowed IPs,
  endpoint, last handshake). Reading it usually needs root; Nodo then shows the
  `sudo wg show all dump > ~/nodo-wg.txt` command and imports that file.
- **Inventory**: name, type (router, switch, access point, server, NAS, camera, printer, TV,
  speaker, phone, tablet, laptop, desktop, IoT, VM / instance, VPN), owner, location, tags, notes,
  links (the web UI), custom fields, several IPs and MACs per device. **Saved** devices stay on the
  map (greyed) while offline. A device is **one device across IP changes**: it is recognised by
  MAC, Tailscale node, AWS instance id, WireGuard key or mDNS name before its IP, and an address
  that moved to another machine is taken from the old one. A LAN machine and its Tailscale node
  join when their host name is unambiguous, when the node's LAN endpoint is the machine's address,
  and for this computer itself, so each appears once on both networks. **Merge** and **split** fix the rest by hand.
- **Monitoring while the app is open**: saved devices (or any you choose) are checked every 30 s
  (configurable) with the system `ping`, falling back to a TCP connect to a port the device is
  known to answer on. Latency, uptime this session, last seen; a history per device (discovered,
  address changed, down, back up after N minutes, merged, edited). **Alerts** in the app and as a
  system notification for a **new unknown device on the LAN** (the first discovery of a map is the
  baseline, not a flood) and for a **saved device down for N minutes** (5 by default). Pause it
  any time.
- **Built-in terminal**: a tabbed panel (alacritty's terminal emulator) docked at the bottom or on
  the right, resizable, collapsible to a bar or maximized (⌘T new shell, ⌘W close tab, ⌘J
  show / hide, ⌘⇧↩ maximize). Tabs run your login shell with its environment. **SSH** opens a tab
  running `ssh user@host` (the device's *SSH user*; the host is its MagicDNS name or Tailscale
  address when it is on your tailnet, else its LAN address), **ping / traceroute** run in a tab,
  and *Open terminal here* starts a shell with `NODO_DEVICE_IP`, `NODO_DEVICE_HOST`,
  `NODO_DEVICE_NAME`, `NODO_DEVICE_USER` and `NODO_DEVICE_MAC` set. A device with a session open
  shows a terminal badge on the graph. Prefer *your* terminal app (Terminal, iTerm2, Ghostty, Warp,
  WezTerm, kitty, Alacritty, GNOME Terminal, Konsole, Windows Terminal…)? Pick it in Settings, or
  use *More → SSH in the external terminal app*; Nodo shows the exact command the first time.
- **Quick actions**: open the web UI, VNC (`vnc://`), copy IP or MAC, Wake-on-LAN.
- **Export**: the graph as PNG, PDF (vector) or SVG, a Mermaid diagram, the inventory as CSV or
  JSON. **Import** a CSV inventory (any of name / IP / MAC / type / owner / location / tags /
  notes / links; other columns become custom fields).
- **Maps are files**: *File → New network map…* / *Open…* / *Open Recent*, like a document. A map
  is one SQLite file (rollback journal, no WAL, so it can live in Dropbox, iCloud Drive or a
  Syncthing folder), checked for integrity on open and migrated forward when Nodo's format
  changes. One map holds as many networks as you like.

## Privacy

Everything stays on your computer, in the map file you chose. Discovery reads this computer's own
tables and listens to the LAN; the active scan runs only when you click it. Tailscale, AWS and
WireGuard are read through their own CLIs. The only request Nodo makes on its own is reading the
project's GitHub star count (`api.github.com/repos/otherwisetech/nodo-rs`, no login, nothing about
you or your networks) at most once a day; turn it off in the globe menu or in Settings. On macOS
the first discovery asks for *Local Network* access.

## The `nodo` command line

Same engine, no window — handy for scripts and AI agents. Without `--map` it uses the map you
opened last in the app.

```sh
nodo list --json                      # every device of the map, with addresses and identities
nodo show 192.168.1.10                # one device (id, name, IP, MAC or host name) + history
nodo show nas --json
nodo scan --passive                   # neighbour table + mDNS + SSDP, printed (writes nothing)
nodo scan --passive --tailscale --save    # …and merge the result into the map
nodo scan --aws my-sso-profile --region us-east-1 --json
nodo export mermaid --layout subnet   # or svg, pdf, csv, json; -o FILE
```

## Download

Ready-made builds for macOS (Apple silicon and Intel), Linux and Windows are attached to each
[GitHub Release](https://github.com/otherwisetech/nodo-rs/releases).

- **macOS:** unzip and drag `Nodo.app` to Applications. Not notarized yet: the first time,
  right-click it → *Open* (or `xattr -dr com.apple.quarantine /Applications/Nodo.app`). The `nodo`
  CLI is inside: `/Applications/Nodo.app/Contents/MacOS/nodo`.
- **Linux:** extract the `.tar.gz` and run `./nodo-app` (`./nodo` for the CLI); copy
  `nodo.desktop` and the icon to `~/.local/share/applications` / `~/.local/share/icons` for a menu
  entry. Needs Vulkan.
- **Windows:** unzip and run `nodo-app.exe` (SmartScreen may ask to confirm, since it is unsigned).

## Build and run

```sh
cargo run -p nodo-app -- ~/Documents/home.nodo   # opens (or creates) that map; no argument: welcome screen
cargo run -p nodo-cli -- scan --passive          # the CLI
script/bundle-macos.sh                           # macOS: target/release/Nodo.app with the icon
UNIVERSAL=1 script/bundle-macos.sh               # Apple silicon + Intel, joined with lipo
python3 script/make-icons.py                     # redraw the icon (Pillow; iconutil on macOS)
python3 script/update-oui.py oui.csv             # refresh the embedded MAC vendor table
```

## Layout

```
crates/core   nodo-core: map file (SQLite), discovery parsers and runners, identity merge,
              graph + layout, monitor engine, exports — no UI
crates/i18n   nodo-i18n: embedded catalogs (es, en, pt, fr) and i18n guard tests
crates/cli    `nodo` command line
crates/app    `nodo-app` desktop app (Nodo.app on macOS)
```

## License

MIT or Apache-2.0, at your option.

Developed by [Otherwise Technologies LLC](https://otherwisetech.com), Wyoming, USA.

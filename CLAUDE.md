# nodo-rs — for agents and contributors

Same conventions as rama-rs, genealogy-rs and diskmap.

## Layout

```
crates/core   nodo-core (lib `nodo_core`), no UI:
              model.rs (networks, devices, identities, observations), db.rs (SQLite map file:
              migrations, integrity check, application id), doc.rs (an open map: apply discovery,
              edits, merge / split, import), identity.rs (observation → device, merge, split),
              discover/ (arp, netinfo, mdns, ssdp, scan, tailscale, aws, wireguard, classify:
              each a pure parser tested on fixtures plus a thin runner), probe.rs (ping / TCP),
              monitor.rs (pure state machine + Engine thread), graph.rs + layout.rs (tree and
              positions: radial, by subnet, by network), export.rs (CSV / JSON / Mermaid, CSV
              import), render.rs (scene → SVG / PDF), mac.rs (+ data/oui.tsv.gz), tools.rs
              (ping / traceroute commands), remote.rs (SSH host / user / argv, device env), wol.rs,
              paths.rs (config dir, recent maps incl. missing ones, default folder:
              `<Otherwise data root>/nodo`), mapfile.rs (safe copy / move via VACUUM INTO +
              verify; cloud-folder detection, conflict names and the `<map>.lock.json` lock are
              otherwise-link's `cloud` / `lock`, so Otherwise Hub sees open maps), shell_env.rs, this_computer.rs (which device
              is the computer running Nodo: interfaces, host name, Tailscale node; runtime only; its VM
              bridges), vms.rs (VM / container runtimes running here, guest names), services.rs
              (HTTP(S) / TCP / DNS / ICMP checks, TLS expiry from DER, Tracker state machine,
              hourly buckets, Engine), share/ (file sharing, see below); otherwise-link
              integration: links.rs (deep-link routes, links into Vigía), feed.rs (Today
              events, live or from the file), sync.rs (devices / sites → shared registry),
              background.rs (Otherwise Helper's monitor job)
crates/i18n   nodo-i18n: embedded translation catalogs, t!/tn!/tv! macros, i18n guard tests
crates/cli    `nodo`: list / show / scan / export / maps / map move / today / sync / mcp (mcp.rs: stdio MCP
              server, read-only unless --allow-writes or --propose), on the same core
crates/app    `nodo-app` (GPUI + gpui-component). app.rs: window, files, top bar, footer, About;
              map_view.rs: toolbar, graph canvas (rama's worktree-map approach), list;
              panel.rs: side panel, quick actions, edit form; discovery.rs: background
              discovery, monitor engine polling, alerts, notifications; dialogs.rs; export_ui.rs;
              terminal/: embedded terminal (from plate: alacritty_terminal model, GPUI view, keys,
              colours; state.rs = pure dock / tab state; panel.rs = dock UI + SSH / ping tabs);
              launch.rs: external terminals (from rama); star.rs: GitHub star (from rama);
              services_ui.rs: Status view, service dialog, badges; share_ui.rs: file sharing;
              otherwise.rs: link pump, Today feed of all windows, registry sync, Vigía links
              mapfile_ui.rs: Save As / Move / Rename / Show in Finder, recent maps' menu,
              lock heartbeat and the open-elsewhere / conflict-copy notices
```

## Rules

- **Passive by default, active only on an explicit action.** Refresh = neighbour table, mDNS,
  SSDP, plus `tailscale status --json` when the CLI is found (local daemon only; run it with
  `TAILSCALE_BE_CLI=1`, or the macOS app binary started without TERM opens its GUI and prints
  nothing). The subnet scan runs only from *Scan subnet…* after the confirmation dialog with the
  "only scan networks you own or administer" warning; private ranges only (`ScanOptions::
  allow_public` stays false in the app), rate-limited, /24 at most. Never add a background scan.
- **Nothing leaves the computer** except the optional once-a-day star count. AWS goes through the
  user's `aws` CLI (never the SDK, never credentials in Nodo); Tailscale and WireGuard through
  their CLIs. SSDP descriptions are fetched from private addresses only.
- **File sharing is opt-in and peer-to-peer.** `core/share/` (crypto.rs: handshake + pinned
  Ed25519 keys + SAS pairing code, all from `ring`; transfer.rs: offer / accept-with-offset /
  chunks / SHA-256; mod.rs: receiver, mDNS `_nodo._tcp`, zip, unique names, quarantine) and
  `app/share_ui.rs`. The receiver listens (port 47821) only while *Receive files* is on; every
  received name goes through `share::safe_name` / `unique_path`; nothing is sent without a user
  action. Tests run the whole protocol on loopback.
- **The map file is the user's document.** Every change is written right away (no save step) in
  small transactions; `journal_mode=DELETE` so it can sit in a synced folder. Never delete or replace a map file
  except through `mapfile` (copy into a temp file, `Store::verify`, rename; delete the old one only
  after the copy reopened). Schema changes are a
  new entry at the end of `db::MIGRATIONS` (never edit a shipped one) plus a test in `db.rs`.
  Monitor results are kept in memory; only state changes become events and `last_seen` is written
  at most every 10 minutes per device.
- **Monitoring runs only while a map is open** (`discovery.rs` starts `monitor::Engine`, polls its
  channel once a second, stops it on close). The engine and `Monitor` state machine are UI-free on
  purpose: a future menu-bar / background monitor should reuse `nodo_core::monitor::Engine` +
  `Doc` from its own small binary (or the CLI) rather than keep the window alive. With the app
  closed, Otherwise Helper runs `nodo background run monitor` every 5 minutes (core
  `background.rs`, cli `background.rs`; declared in the bundle's
  `Contents/Resources/otherwise-background.json`): the most recent map, **read-only**, its saved
  devices with monitoring on (ping, then their known ports; a miss is retried once) and its
  enabled service checks — no discovery, no scan. Up / down per device and service lives in
  its own `<data root>/nodo/background.json`; the first round is a baseline, then each change
  is one notification.
  Services (`services::Engine`) start and stop with it, share its pause, and write only hourly
  buckets (at most every 10 minutes) and incidents to the map (migration 3).
- **Otherwise Hub integration is opt-in or read-only.** The registry is touched only when
  `hub.json` has `entities: true` (`Registry::open_shared`) and only with saved devices;
  `nodo mcp` writes only with `--allow-writes`; links into Vigía only open its form. Tests use
  `Root::at(tempdir)`, never the real shared folder. otherwise-link is a git tag; bumping it
  must only change its own Cargo.lock entry.
- **i18n is mandatory.** No user-facing string literal in `app/src`, `core/src` or `cli/src`; use
  `i18n::t!` / `tn!` and add the key to **every** `crates/i18n/locales/<lang>/` catalog in the same
  change. Untranslatable literals end their line with `// i18n-ok`.
- Every core change ships with a test on synthetic data: parsers on fixtures (arp per OS, Tailscale
  JSON, AWS JSON, wg dump, SSDP / mDNS records), OUI lookup, identity merge, layout determinism and
  overlaps, monitor state machine, migrations. **No real network in tests** (use the `Prober`
  trait's fakes). Tests must pass on macOS, Linux and Windows: no assumptions about installed
  tools, paths or locale.
- **The assistant is the shared AI panel** (`otherwise-agent-panel`, `app/src/assistant.rs`,
  ⌘J; the embedded terminal's toggle is ⌃` on macOS): the user's Claude Code / Codex / opencode
  with `nodo mcp --map <this window's map>` for that session only (never write the user's agent
  configs). The panel never gets `--allow-writes`: outside "Solo lectura" it starts `--propose
  <app dir>/assistant/live/<pid>-<window>/proposals`, where `update_device` / `add_service` only
  validate and drop a `nodo_core::agent::Proposal`; the window polls it, shows cards and applies
  them with the same builders (`agent::edited_device`, `new_service`) on Aplicar. A map open on
  another computer gets read tools only.
- In app test modules don't `use super::*` (gpui's glob exports its own `test` attribute).
- External terminals get the exact command line shown before its first run (remembered per
  terminal + command in settings). Embedded tabs run `ssh` / `ping` with an argument list, never
  through a shell; `remote::ssh_argv` rejects users / hosts that could become options.
- Comments: sparse, only non-obvious why.
- GPUI pins: `Cargo.lock` pins zed `eb5483528be607b497afdaad0a237bebeb191249` and gpui-component
  `972a3ebfd01afca7da6d8b6f31c9a51288ea5565`, as in rama-rs. Never `cargo update` them.

## Release

Releases stay in this repo (`gh release create vX.Y.Z` with each file and its `.sha256`).
Notes, file names and issue forms follow the family's release rules: otherwisetech/otherwise-apps
`docs/releases.md` (how the Hub discovers and verifies releases).

## Validate

```
cargo test -p nodo-i18n -p nodo-core -p nodo-cli -p nodo-app
cargo clippy --workspace --all-targets
```

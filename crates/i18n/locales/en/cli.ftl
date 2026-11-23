# The nodo command line.
cli.usage = nodo — Nodo's network maps from the terminal (read-only unless --save)\n\nUsage:\n  nodo list [--map FILE] [--json]          devices of a map\n  nodo show <device> [--map FILE] [--json] one device (id, name, IP, MAC or host name) with its history\n  nodo scan [--passive] [--tailscale] [--wireguard] [--aws PROFILE [--region R]…] [--json] [--save] [--map FILE]\n                                           discover now (passive by default: neighbour table, mDNS, SSDP); --save writes into the map\n  nodo export <svg|pdf|mermaid|csv|json> [--layout radial|subnet|network] [-o FILE] [--map FILE]\n  nodo today [--map FILE] [--json]         what needs attention (devices and services down, certificates expiring)\n  nodo sync [--map FILE]                   publish saved devices and sites to the shared registry (when on in Otherwise Apps)\n  nodo mcp [--map FILE] [--allow-writes]   MCP server on stdin/stdout (read-only unless --allow-writes)\n                                           export the graph or the inventory\n  nodo maps                                recently opened maps\n  nodo map move <FROM> <TO> [--force]    move a map file safely (copy, check, then delete the original)\n  nodo services [list] [--json]           monitored services with uptime (24 h / 7 d / 30 d)\n  nodo services add <http|tcp|dns|icmp> <target> [--name N] [--device D] [--interval S] [--keyword K] [--expect CODE] [--slow MS] [--cert-days N]\n  nodo services check [id | name] [--json] run checks now (exit 1 if any fails)\n\nWithout --map, the most recently opened map is used.
cli.err.needs_value = {flag} needs a value
cli.err.no_map = No map given and no recent map. Pass --map FILE or open one in the Nodo app first.
cli.err.no_device = No device matches "{query}".
cli.err.ambiguous = "{query}" matches {n} devices; use its id, IP or MAC.
cli.err.show_needs = Which device? nodo show <id | name | IP | MAC>
cli.err.unknown = Unknown command "{command}". Run nodo --help.
cli.list.header = {map} — {n} devices
cli.col.name = NAME
cli.col.type = TYPE
cli.col.seen = LAST SEEN
cli.col.vendor = vendor
cli.col.hostname = host name
cli.col.os = OS
cli.col.model = model
cli.col.owner = owner
cli.col.location = location
cli.col.tags = tags
cli.col.service = service
cli.col.notes = notes
cli.scan.passive = Listening for a few seconds (neighbour table, mDNS, SSDP)…
cli.scan.saved = Saved into {map}: {new} new, {updated} updated.
cli.err.format = Unknown export format "{format}": use svg, pdf, mermaid, csv or json.
cli.col.kind = KIND
cli.col.target = TARGET
cli.col.uptime = UPTIME 24h / 7d / 30d
cli.svc.add_needs = Usage: nodo services add <http|tcp|dns|icmp> <target>
cli.svc.added = Added service {id}: {name}
cli.svc.none = No such service.
cli.svc.cert = (certificate: {days} d)

# Otherwise Apps: Today view, shared registry, MCP
cli.today.none = Nothing needs attention.
cli.sync.off = The shared registry is off. Turn it on in Otherwise Apps.
cli.sync.done = Shared registry: {pushed} sent, {unlinked} unlinked.
cli.map.needs = Usage: nodo map move <from> <to> [--force]
cli.map.moved = Moved {from} → {to}

# background job (Otherwise Helper)
cli.bg.monitor_label = Device and service monitor
cli.bg.unknown_job = Unknown background job
cli.bg.failed = couldn't read the map: {error}
cli.bg.device_down = {device} is down
cli.bg.device_up = {device} is back up
cli.bg.service_down = {service} is failing
cli.bg.service_up = {service} is working again
cli.bg.down_status.one = {n} down
cli.bg.down_status.other = {n} down
cli.bg.up_status.one = {n} up
cli.bg.up_status.other = all {n} up

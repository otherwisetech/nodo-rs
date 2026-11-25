# La línea de comandos nodo.
cli.usage = nodo — los mapas de red de Nodo desde la terminal (solo lectura salvo --save)\n\nUso:\n  nodo list [--map ARCHIVO] [--json]          dispositivos de un mapa\n  nodo show <dispositivo> [--map ARCHIVO] [--json] un dispositivo (id, nombre, IP, MAC o nombre de host) con su historial\n  nodo scan [--passive] [--tailscale] [--wireguard] [--aws PERFIL [--region R]…] [--json] [--save] [--map ARCHIVO]\n                                              descubrir ahora (pasivo por defecto: tabla de vecinos, mDNS, SSDP); --save lo guarda en el mapa\n  nodo export <svg|pdf|mermaid|csv|json> [--layout radial|subnet|network] [-o ARCHIVO] [--map ARCHIVO]\n  nodo today [--map ARCHIVO] [--json]      lo que requiere atención (equipos y servicios caídos, certificados por vencer)\n  nodo sync [--map ARCHIVO]                publica equipos guardados y sitios en el registro compartido (si está activo en Otherwise Hub)\n  nodo mcp [--map ARCHIVO] [--allow-writes | --propose CARPETA]  servidor MCP por stdin/stdout (solo lectura; --propose deja los cambios como propuestas para la app)\n                                              exporta el grafo o el inventario\n  nodo maps                                   mapas abiertos recientemente\n  nodo map move <ORIGEN> <DESTINO> [--force] mueve un mapa con seguridad (copia, comprueba y luego borra el original)\n  nodo services [list] [--json]           servicios monitorizados con disponibilidad (24 h / 7 d / 30 d)\n  nodo services add <http|tcp|dns|icmp> <destino> [--name N] [--device D] [--interval S] [--keyword K] [--expect CÓDIGO] [--slow MS] [--cert-days N]\n  nodo services check [id | nombre] [--json] comprueba ahora (sale con 1 si alguno falla)\n\nSin --map se usa el último mapa abierto.
cli.err.needs_value = {flag} necesita un valor
cli.err.no_map = No se indicó mapa y no hay ninguno reciente. Usa --map ARCHIVO o abre uno antes en la app Nodo.
cli.err.no_device = Ningún dispositivo coincide con "{query}".
cli.err.ambiguous = "{query}" coincide con {n} dispositivos; usa su id, IP o MAC.
cli.err.show_needs = ¿Qué dispositivo? nodo show <id | nombre | IP | MAC>
cli.err.unknown = Comando desconocido "{command}". Ejecuta nodo --help.
cli.list.header = {map} — {n} dispositivos
cli.col.name = NOMBRE
cli.col.type = TIPO
cli.col.seen = VISTO
cli.col.vendor = fabricante
cli.col.hostname = nombre de host
cli.col.os = SO
cli.col.model = modelo
cli.col.owner = responsable
cli.col.location = ubicación
cli.col.tags = etiquetas
cli.col.service = servicio
cli.col.notes = notas
cli.scan.passive = Escuchando unos segundos (tabla de vecinos, mDNS, SSDP)…
cli.scan.saved = Guardado en {map}: {new} nuevos, {updated} actualizados.
cli.err.format = Formato de exportación desconocido "{format}": usa svg, pdf, mermaid, csv o json.
cli.col.kind = TIPO
cli.col.target = DESTINO
cli.col.uptime = DISPONIBILIDAD 24h / 7d / 30d
cli.svc.add_needs = Uso: nodo services add <http|tcp|dns|icmp> <destino>
cli.svc.added = Servicio {id} añadido: {name}
cli.svc.none = No existe ese servicio.
cli.svc.cert = (certificado: {days} d)

# Otherwise Hub: Today view, shared registry, MCP
cli.today.none = Nada requiere atención.
cli.sync.off = El registro compartido está desactivado. Actívalo en Otherwise Hub.
cli.sync.done = Registro compartido: {pushed} enviados, {unlinked} desvinculados.
cli.map.needs = Uso: nodo map move <origen> <destino> [--force]
cli.map.moved = Movido {from} → {to}

# trabajo en segundo plano (Otherwise Helper)
cli.bg.monitor_label = Monitor de dispositivos y servicios
cli.bg.unknown_job = Trabajo en segundo plano desconocido
cli.bg.failed = no se pudo leer el mapa: {error}
cli.bg.device_down = {device} está caído
cli.bg.device_up = {device} volvió a estar en línea
cli.bg.service_down = {service} está fallando
cli.bg.service_up = {service} vuelve a funcionar
cli.bg.down_status.one = {n} caído
cli.bg.down_status.other = {n} caídos
cli.bg.up_status.one = {n} en línea
cli.bg.up_status.other = los {n} en línea

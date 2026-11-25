# La ligne de commande nodo.
cli.usage = nodo — les cartes réseau de Nodo depuis le terminal (lecture seule sauf --save)\n\nUsage :\n  nodo list [--map FICHIER] [--json]          appareils d'une carte\n  nodo show <appareil> [--map FICHIER] [--json] un appareil (id, nom, IP, MAC ou nom d'hôte) avec son historique\n  nodo scan [--passive] [--tailscale] [--wireguard] [--aws PROFIL [--region R]…] [--json] [--save] [--map FICHIER]\n                                              découvrir maintenant (passif par défaut : table des voisins, mDNS, SSDP) ; --save l'enregistre dans la carte\n  nodo export <svg|pdf|mermaid|csv|json> [--layout radial|subnet|network] [-o FICHIER] [--map FICHIER]\n  nodo today [--map FICHIER] [--json]      ce qui demande attention (appareils et services hors ligne, certificats qui expirent)\n  nodo sync [--map FICHIER]                publie les appareils enregistrés et les lieux dans le registre partagé (s'il est activé dans Otherwise Hub)\n  nodo mcp [--map FICHIER] [--allow-writes | --propose DOSSIER]  serveur MCP sur stdin/stdout (lecture seule ; --propose laisse les changements en propositions pour l'app)\n                                              exporte le graphe ou l'inventaire\n  nodo maps                                   cartes ouvertes récemment\n  nodo map move <SOURCE> <CIBLE> [--force] déplace une carte en sécurité (copie, vérifie, puis supprime l’original)\n  nodo services [list] [--json]           services surveillés avec disponibilité (24 h / 7 j / 30 j)\n  nodo services add <http|tcp|dns|icmp> <cible> [--name N] [--device D] [--interval S] [--keyword K] [--expect CODE] [--slow MS] [--cert-days N]\n  nodo services check [id | nom] [--json] vérifie maintenant (code 1 si l'un échoue)\n\nSans --map, la dernière carte ouverte est utilisée.
cli.err.needs_value = {flag} attend une valeur
cli.err.no_map = Aucune carte indiquée et aucune récente. Utilisez --map FICHIER ou ouvrez-en une dans l'app Nodo.
cli.err.no_device = Aucun appareil ne correspond à « {query} ».
cli.err.ambiguous = « {query} » correspond à {n} appareils ; utilisez son id, son IP ou sa MAC.
cli.err.show_needs = Quel appareil ? nodo show <id | nom | IP | MAC>
cli.err.unknown = Commande inconnue « {command} ». Lancez nodo --help.
cli.list.header = {map} — {n} appareils
cli.col.name = NOM
cli.col.type = TYPE
cli.col.seen = VU
cli.col.vendor = fabricant
cli.col.hostname = nom d'hôte
cli.col.os = OS
cli.col.model = modèle
cli.col.owner = responsable
cli.col.location = emplacement
cli.col.tags = étiquettes
cli.col.service = service
cli.col.notes = notes
cli.scan.passive = Écoute pendant quelques secondes (table des voisins, mDNS, SSDP)…
cli.scan.saved = Enregistré dans {map} : {new} nouveaux, {updated} mis à jour.
cli.err.format = Format d'export inconnu « {format} » : utilisez svg, pdf, mermaid, csv ou json.
cli.col.kind = TYPE
cli.col.target = CIBLE
cli.col.uptime = DISPONIBILITÉ 24h / 7j / 30j
cli.svc.add_needs = Usage : nodo services add <http|tcp|dns|icmp> <cible>
cli.svc.added = Service {id} ajouté : {name}
cli.svc.none = Ce service n'existe pas.
cli.svc.cert = (certificat : {days} j)

# Otherwise Hub: Today view, shared registry, MCP
cli.today.none = Rien ne demande attention.
cli.sync.off = Le registre partagé est désactivé. Activez-le dans Otherwise Hub.
cli.sync.done = Registre partagé : {pushed} envoyés, {unlinked} détachés.
cli.map.needs = Utilisation : nodo map move <source> <cible> [--force]
cli.map.moved = Déplacé {from} → {to}

# tâche en arrière-plan (Otherwise Helper)
cli.bg.monitor_label = Surveillance des appareils et services
cli.bg.unknown_job = Tâche en arrière-plan inconnue
cli.bg.failed = lecture de la carte impossible : {error}
cli.bg.device_down = {device} ne répond plus
cli.bg.device_up = {device} répond de nouveau
cli.bg.service_down = {service} est en échec
cli.bg.service_up = {service} fonctionne de nouveau
cli.bg.down_status.one = {n} hors ligne
cli.bg.down_status.other = {n} hors ligne
cli.bg.up_status.one = {n} en ligne
cli.bg.up_status.other = les {n} en ligne

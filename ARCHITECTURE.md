# rsvga : architecture

## But

Donner l'accélération 3D DirectX 9 à 11 aux VM Windows sous QEMU/KVM, **sans céder le GPU** (pas de passthrough). Côté Windows, on utilise le **pilote VMware SVGA officiel et signé**, sans mode test ni pilote noyau maison.

## Principe

```
Windows invité  : pilote VMware SVGA officiel (inchangé)
      │  registres, FIFO, command buffers (mémoire partagée)
QEMU (≥ 10.1)   : -device vfio-user-pci,socket=/run/rsvga.sock
      │  protocole vfio-user (socket Unix + mémoire partagée)
rsvga-server    : processus séparé, sandboxé, en Rust
      │  Vulkan (ash)
GPU de l'hôte   : RX 6950 XT, via /dev/dri/renderD*
```

- rsvga implémente la carte **VMware SVGA II** (PCI `15ad:0405`) dans un processus à part. QEMU s'y branche par **vfio-user**, intégré à QEMU depuis la 10.1 : il n'y a pas de gros patch dans QEMU.
- Le rendu se fait **directement en Vulkan**. DXVK n'est pas utilisé, donc l'état graphique n'est pas suivi deux fois comme dans qemu-vmvga.
- La mémoire de l'invité est partagée en memfd (`memory-backend-memfd,share=on`), ce que la VM Tiny11 fait déjà pour virtiofs.

## Composants (crates)

| Crate | Rôle | Jalon |
|---|---|---|
| `svga-proto` | Constantes du protocole VMware, transcrites des en-têtes MIT du *SVGA Device Developer Kit*. `no_std`, sans code. | M0 |
| `svga-core` | Logique pure du périphérique : FIFO, décodage des commandes (2D, objets guest-backed, DX), registres, objets. Sans I/O et sans `unsafe`. | M0 → M3 |
| `svga-render` | Moteur Vulkan et traducteur de shaders VGPU10 (bytecode SM4/SM5) vers SPIR-V. | M3 → M4 |
| `vfio-user` | Protocole vfio-user côté serveur : messages, passage de descripteurs, boucle de requêtes. Sans `unsafe`. | M1 |
| `rsvga-server` | Binaire : config PCI, BAR partagés (memfd scellés), ROM optionnelle, exécution de la FIFO, capture PNG, fenêtre. Seul `shm.rs` contient de l'`unsafe`. | M1 |
| `firmware/gop` | Pilote UEFI GOP (`no_std`, 7 Ko) livré dans la ROM de la carte : pilote les vrais registres SVGA et offre un framebuffer linéaire au firmware, au chargeur de Windows et à son pilote d'affichage de base. | M1 |

## Règles de sécurité

1. **Tout ce qui vient de l'invité est hostile.** `svga-core` est en `#![forbid(unsafe_code)]`.
2. **Lecture unique, puis copie, puis validation.** La configuration de la FIFO (MIN, MAX, NEXT_CMD, STOP) est figée au début de chaque passe. Chaque commande est copiée hors de la mémoire partagée avant d'être interprétée : pas de TOCTOU.
3. **Toutes les tailles sont bornées**, en arithmétique 64 bits : curseurs, escapes, commandes 3D ≤ 256 Kio. Une commande plus grande que l'anneau est rejetée, sinon on attendrait indéfiniment.
4. **Une commande incomplète n'est jamais consommée.** Une commande invalide arrête la FIFO et lève `SVGA_IRQFLAG_ERROR`, sans jamais paniquer.
5. **Budget par passe** : l'invité ne peut pas monopoliser le thread de traitement.
6. **`unsafe` confiné** à `rsvga-server` (mmap, accès volatils) et à `svga-render` (Vulkan), derrière des API sûres et documentées.
7. **Sandbox du serveur** : seccomp et landlock. Le serveur n'accède qu'au socket vfio-user, à la mémoire invitée et à `/dev/dri/renderD*`.
8. **Fuzzing de chaque décodeur** (cargo-fuzz), en plus des tests unitaires et d'un test de stress déterministe.

## Feuille de route

| Jalon | Contenu | Critère de réussite |
|---|---|---|
| **M0 : fondations** (fait) | Protocole, FIFO validée, décodeur des commandes 2D et de l'encadrement 3D, tests, fuzzing. | `cargo test` vert, cible de fuzz qui tourne. |
| **M1 : bureau 2D** (atteint) | Serveur vfio-user, registres SVGA, VRAM et FIFO en mémoire partagée, mode graphique, `UPDATE` affiché dans une fenêtre. | OVMF puis Windows affichés avec le pilote VMware, en 2D. |
| **M2 : pilote WDDM** (atteint) | Objets « guest-backed » (MOB, OTables), command buffers, Screen Targets, curseur, fences et IRQ. | Bureau Windows 11 à la résolution native, redimensionnement. |
| **M3 : 3D de base** (en cours) | Contexte DX, ressources, vues, clear/copy/draw, traducteur VGPU10 → SPIR-V (VS/PS). | Bureau DWM en 3D, triangle D3D11, `dxdiag` sans erreur. |
| **M4 : D3D11 complet** | GS/HS/DS/CS, stream output, requêtes, MSAA, capacités FL 11_0. | 3DMark et jeux DX11. |
| **M5 : intégration** | Sandbox, patchs QEMU proposés upstream, paquet Debian. | Installation en une commande. |

## État de M1 (2026-11-06)

Testé avec le QEMU 11.1.2 de trixie-backports (extrait localement, sans installation), q35, KVM et OVMF :

- **Ce qui marche** : QEMU se connecte à `rsvga` et négocie la version. Il lit l'espace PCI, assigne les BAR (I/O en 0x6060, VRAM en 0x80000000, FIFO en 0x84000000) et mappe VRAM et FIFO directement dans le guest via KVM (`info mtree` : `ramd ... KVM`). Il partage la RAM du guest (fenêtres DMA avec fd).
- **Blocage 1, accès non alignés (résolu par patch)** : le protocole SVGA II lit et écrit le port de valeur en `base+1`, en 32 bits, donc non aligné. QEMU rejette ces accès sur les régions vfio, car `vfio_region_ops` (`hw/vfio/region.c`) n'a pas `.valid.unaligned`, alors que sa carte `vmware-svga` intégrée l'a. Vérifié au moniteur : les écritures d'index (alignées) arrivent, celles du port de valeur jamais. Solutions :
  1. un patch QEMU d'une ligne (`.unaligned = true` pour les régions vfio), à proposer upstream ;
  2. émuler plutôt **SVGA3** (PCI `15ad:0406`), dont les registres sont en MMIO aligné.

  Vérifié dans VMware Tools 13.1.5 (build 25544008) : l'INF du pilote Windows 10/11 x64 (`vm3d.inf`, 9.17.11.0004) ne déclare que `PCI\VEN_15AD&DEV_0405`. Seul l'ancien paquet Windows 7 (9.17.06.0005, 2023) déclare aussi `DEV_0406`. **Décision : SVGA II d'abord (avec le patch QEMU), SVGA3 ensuite comme second modèle**, utile pour Linux (`vmwgfx` gère les deux) et peut-être pour Windows avec ce vieux pilote. Le cœur reste commun aux deux modèles.

  **Résolu** par [`patches/qemu/0001-vfio-user-pass-unaligned-region-accesses-through.patch`](patches/qemu/0001-vfio-user-pass-unaligned-region-accesses-through.patch) (33 lignes). Le transport vfio-user déclare une nouvelle capacité `VFIO_IO_CAP_UNALIGNED` et ses régions acceptent les accès non alignés ; le vfio du noyau ne change pas. `scripts/build-qemu.sh` construit ce QEMU dans `target/qemu/install`. `scripts/smoke-qemu.py` le vérifie : avec le QEMU non patché, chaque lecture par le port de valeur renvoie 0 ; avec le QEMU patché, l'ID, la VRAM, le changement de mode 800×600×32 et la capture sont corrects.
- **Blocage 2, firmware (résolu)** : l'OVMF de Debian (edk2 2025.02) pilote les cartes VMware comme des cartes Bochs, via les ports VBE 0x1CE/0x1CF (`QemuVideoDxe/Driver.c` : « VMWare SVGA is handled like Bochs »), et vfio ne transmet pas ces ports. **Résolu** par notre propre pilote GOP (`firmware/gop`), empaqueté en ROM optionnelle PCI (`rsvga-server/src/rom.rs`) et servi par la région 6 de vfio-user, avec le BAR ROM dans l'espace de config. OVMF le charge depuis la carte (pilote spécifique au bus, prioritaire), et la console UEFI s'affiche sur rsvga : voir [docs/img/m1-ovmf-shell.png](docs/img/m1-ovmf-shell.png).
- **Windows** : Tiny11 (Windows 11 25H2) démarre sur rsvga (testé sur une couche qcow2 temporaire, disque d'origine intact). Le chargeur puis le pilote « Microsoft Basic Display Adapter » utilisent le framebuffer de notre GOP : le bureau s'affiche en 1280×800 au bout de 2 minutes, sans pilote VMware.
- **Pilote VMware (M1 atteint)** : le pilote officiel `vm3d` 9.17.11 (VMware Tools 13.1.5), installé par `pnputil`, se charge sur rsvga. Il négocie l'ID, démarre la FIFO, initialise les GMR par `GMR_DESCRIPTOR`, puis affiche le bureau en 1024×768 avec seulement `UPDATE` et `FENCE` (chemin framebuffer). Il **exige les interruptions** : sans elles, il tourne indéfiniment en `SYNC`/`BUSY`, car il attend la fence dans son gestionnaire d'interruption. rsvga fournit donc l'INTx de vfio-user, avec la sémantique de vfio-pci : eventfd de déclenchement, masquage automatique, démasquage par l'eventfd de rééchantillonnage de KVM, statut et acquittement par le port 8. Mesuré : 2 131 commandes, 319 fences, 319 interruptions acquittées, extinction ACPI propre en 3 s.
- **Prêts mais pas encore annoncés au pilote** : la RAM du guest (fenêtres DMA mappées et scellées), les GMR2 et les Screen Objects avec leurs blits (`twod.rs`). Ils serviront dès qu'on annoncera `SVGA_CAP_GMR2` et `SCREEN_OBJECT_2` (M2).
- **Interaction** : la fenêtre de rsvga (winit + softbuffer) envoie clavier (par position physique) et souris (tablette absolue) à QEMU par QMP. Validé en usage réel, curseur matériel de Windows compris.

## État de M2 (atteint le 2026-11-07)

- **Screen Objects (fait, vérifié sur Windows)** : rsvga annonce `SVGA_CAP_GMR2` et `SCREEN_OBJECT_2`. Le pilote `vm3d` passe alors en mode Screen Object : il décrit sa mémoire par `DEFINE_GMR2` / `REMAP_GMR2`, crée l'écran par `DEFINE_SCREEN`, puis pousse les pixels par `DEFINE_GMRFB` + `BLIT_GMRFB_TO_SCREEN`. Résultat : bureau Windows 11 en 1920×1080, sans erreur, environ 3 % d'un cœur au repos. Un écran redéfini garde ses pixels, et la fenêtre ne recopie que les lignes modifiées. Le pilote fournit aussi une zone de sauvegarde en VRAM pour chaque écran ; rsvga l'accepte sans s'en servir, car ses pixels sont en mémoire hôte.
- **Command buffers (faits, utilisés par Windows)** : `SVGA_CAP_COMMAND_BUFFERS` et `CMD_BUFFERS_2`. L'en-tête `SVGACBHeader` est lu en RAM invitée. Il y a deux contextes et le contexte « device » (start/stop, preempt, vider la file). Un contexte arrêté met les buffers en file. Une erreur arrête le contexte, ce qu'attend vmwgfx, qui le relance ensuite. Le statut et `errorOffset` sont réécrits, puis l'IRQ `COMMAND_BUFFER` est levée. L'exécution est synchrone, pendant l'écriture de `COMMAND_LOW` : les commandes du contexte « device » doivent être finies au retour. En mode guest-backed, le pilote envoie presque tout par ce canal.
- **Le pilote Windows demande d'abord à l'hyperviseur** : avant d'initialiser la carte, il appelle **`BDOOR_CMD_GET_SVGA_CAPABILITIES`** (commande 75 du port VMware, mode noyau seulement). Le type de capacité est dans le mot haut de `ECX` (`SVGABackdoorCapType` dans `svga_reg.h` : 0 = capacités de la carte, 1 = capacités FIFO, 2 = version matérielle 3D, 3 = `CAP2`...). En cas de succès, `EBX` = magic et `EAX` = valeur. Sinon `EAX` = -1 et `EBX` reste inchangé. Il n'appelle que le type 0. Le QEMU officiel répond « non géré », et le pilote se limite alors à la 2D, quoi que la carte annonce.
- **Patch QEMU 0002** ([patches/qemu/0002-vmport-answer-SVGA-capability-queries-and-GuestRPC.patch](patches/qemu/0002-vmport-answer-SVGA-capability-queries-and-GuestRPC.patch)) : deux ajouts au port VMware, désactivés par défaut. (1) `svga-backdoor` + `svga-caps`, `svga-fifo-caps`, `svga-3d-hwversion`, `svga-caps2` : la réponse à la commande 75. Les valeurs viennent de rsvga (`rsvga --vmport-args`, que `run-windows.sh` passe à QEMU) : elles sont donc toujours celles de la carte. (2) `guestrpc` : un canal GuestRPC « RPCI » minimal (cookies, envoi et réception 4 octets par 4 comme open-vm-tools). Il trace les messages, répond « OK » à `log` et « commande inconnue » à ce qu'il ne connaît pas. `info-get` et `info-set` passent par une petite mémoire de clés, l'équivalent des lignes `guestinfo.*` d'un `.vmx` : la propriété `guestinfo` (option `--guestinfo` de `run-windows.sh`) donne les valeurs de départ, et une clé absente reçoit « 0 No value found », comme sous VMware.
- **Le pilote se raconte** : par GuestRPC, `vm3d` journalise chaque décision et lit ses réglages (`info-get guestinfo.svga.wddm.*`, `guestinfo.loglevel.vm3d.*`). Avec `--trace`, tout arrive dans `target/vm/qemu.log` (trace `vmport_guestrpc_message`). C'est ce journal qui a guidé la suite : « 3D is off », « Guest backed surface is off », « requested 3D, but device does not meet minimum capability requirements (SM2) »...
- **Ce que vérifie le pilote** (établi avec `RSVGA_PROBE_CAPS`, `RSVGA_PROBE_CAPS2`, `RSVGA_PROBE_HW3D` : annonces sans implémentation, pour le diagnostic seulement) :
  - **La 3D** dépend de `SVGA_CAP_3D`, de la version matérielle 3D dans la FIFO et du **bloc de capacités 3D de la FIFO** (`SVGA_FIFO_3D_CAPS`, enregistrements `{longueur, type 0x100}` puis paires `(index, valeur)`) : le pilote ne lit pas `DEV_CAP` pour décider. Avec `SVGA3D_DEVCAP_3D = 1`, il passe « 3D is on » ; il exige aussi au moins le shader model 2 (« minimum capability requirements (SM2) »).
  - **Les objets guest-backed** demandent `SVGA_CAP_GBOBJECTS`, `CMD_BUFFERS_2` et le registre `CAP2` (et la réponse « No value found » de GuestRPC). Il lit alors les 262 `DEV_CAP`, `GBOBJECT_MEM_SIZE_KB`, `MOB_MAX_SIZE`, `SCREENTARGET_MAX_*`.
  - Avec la 3D annoncée sans la fournir, l'écran reste noir : on n'annoncera la 3D qu'avec un vrai rendu (M3, voir plus bas).
- **Objets guest-backed (fait, vérifié sur Windows)** : rsvga annonce `SVGA_CAP_GBOBJECTS`, `CMD_BUFFERS_2`, `CAP2_REGISTER` et `CAP2` = `GROW_OTABLE | GB_MEMSIZE_2 | OTABLE_PTDEPTH_2`, sans 3D. Le pilote se charge en « Display Only driver » avec « Guest backed surface is on », et affiche le bureau par **Screen Targets** : `SET_OTABLE_BASE64`, `DEFINE_GB_MOB64`, `ENABLE_GART`, `DEFINE_GB_SURFACE_V4`, `BIND_GB_SURFACE_WITH_PITCH`, `DEFINE/BIND_GB_SCREENTARGET`, puis `UPDATE_GB_IMAGE` + `UPDATE_GB_SCREENTARGET` à chaque changement, tout par command buffers. Bureau Windows 11 en 1920×1080, Explorateur et déplacements de fenêtres corrects, aucune commande en erreur ni inconnue.
  - `svga-core` : `gb.rs` décode les commandes, `layout.rs` calcule la disposition d'une surface comme vmwgfx (couches, puis niveaux de mip, en blocs du format), `mob.rs` lit les tables de pages des MOB (profondeur 0 à 2, entrées 32 ou 64 bits). La table des 153 formats (`svga-proto/src/format.rs`) est générée depuis `svga3d_surfacedefs.h` par `scripts/gen-surface-formats.py`.
  - `rsvga-server/src/gb.rs` : la liste des pages d'un MOB est lue une fois, à sa définition. Chaque surface a une copie « device » de même disposition que sa copie invitée, créée au premier besoin ; `UPDATE` copie MOB → device, `READBACK` device → MOB, `SURFACE_COPY` copie entre copies device. Un pas de ligne propre au MOB (`WITH_PITCH`) est respecté. `UPDATE_GB_SCREENTARGET` recopie la zone vers les pixels de l'écran affiché, et la fenêtre n'en reprend que la partie modifiée. Les tables d'objets sont seulement mémorisées : rsvga garde l'état de ses objets chez lui.
  - Limites : 1 Gio de pages de MOB, 256 Mio par MOB, 1 Gio de copies device, 16 Screen Targets.
- **Redimensionnement depuis l'hôte (fait)** : comme sous VMware, il passe par les VMware Tools de l'invité. L'hôte leur envoie `Resolution_Set <largeur> <hauteur>` sur le canal GuestRPC « TCLO » (hôte → invité). Le patch 0002 gère ce canal : il accueille chaque canal par `reset` puis `Capabilities_Register` (les Tools n'annoncent leurs capacités que sur demande) et livre une commande en attente à chaque interrogation. Il ajoute aussi la commande QMP `x-vmport-guestrpc-send`. La fenêtre de rsvga envoie la taille choisie 300 ms après le dernier redimensionnement, bornée entre 800×600 et le maximum de la carte ; quand Windows adopte la taille, la fenêtre y est déjà et rien ne repart. Vérifié : `Resolution_Set 1600 900` envoyé par QMP fait passer Windows en 1600×900 en 2 s environ ; puis, fenêtre redimensionnée à la main, rsvga envoie `Resolution_Set 1738 1002` et `1834 1066`, et Windows redéfinit son Screen Target exactement à ces tailles. **M2 est atteint.**
- **Installer les VMware Tools** (dans l'invité, en plus du pilote) demande deux choses : (1) sur l'hôte, `options kvm enable_vmware_backdoor=Y` (`/etc/modprobe.d/`, puis recharger `kvm` toutes VM arrêtées), sans quoi les programmes en mode utilisateur n'atteignent pas le port VMware ; (2) la signature d'hyperviseur de VMware dans le CPUID, que `run-windows.sh` met par défaut (`-cpu host,kvm=off,hv-relaxed,hv-vendor-id=VMwareVMware`, `--kvm-cpu` pour revenir à KVM) : sinon l'action `VM_CheckRequirements` de l'installeur annule tout (code 1602). Installation silencieuse : `setup.exe /S /v "/qn REBOOT=R"`, puis un redémarrage (pilote souris VMware). Les Tools s'annoncent alors : `tools.capability.resolution_set 1`, `display_topology_set 2`, `resolution_server toolbox 1`...
- **Fast Startup** : un Windows arrêté normalement se met en veille prolongée. Au démarrage suivant, son pilote reprend avec les choix faits face à l'ancienne carte, sans relire les capacités. `run-windows.sh` cache donc S3 et S4 à l'invité (`-global ICH9-LPC.disable_s3=1 -global ICH9-LPC.disable_s4=1`) : chaque démarrage est complet.
- **Outils de test** : `run-windows.sh` ouvre un moniteur QEMU (HMP) sur `$XDG_RUNTIME_DIR/rsvga-vm-hmp.sock`. On peut y lire la position du curseur dans la FIFO (`xp` sur le BAR 2), déplacer la souris PS/2, envoyer des touches ou activer des traces.

## M3 : la 3D (en cours)

### Ce qui est établi (2026-11-07)

La 3D reste derrière les variables de sonde (`RSVGA_PROBE_CAPS=9dc6c222 RSVGA_PROBE_CAPS2=2d RSVGA_PROBE_HW3D=20001`) tant que rsvga n'exécute pas les commandes DX. Avec elles, **DWM tourne sur notre carte en DirectX 10.1** : il définit un contexte DX et envoie des milliers de commandes (shaders, états, vues, `DX_DRAW_INDEXED`...). L'écran reste noir, puisqu'elles ne sont pas encore exécutées. Le chemin pour y arriver :

- **Pourquoi DWM échouait** : le pilote DirectX en mode utilisateur (`vm3dum64_10.dll`) contient, pour chaque feature level (11_1, 11_0, 10_1, 10_0) et chaque version de DDI, la **valeur minimale de chaque devcap** qu'il utilise. Si aucun niveau n'est satisfait, il n'en annonce aucun : `D3D11CreateDevice` échoue avec `DXGI_ERROR_UNSUPPORTED` à tous les niveaux, DWM se rabat sur WARP, que Windows refuse sur un adaptateur 3D, et plante en boucle (`dwmcore.dll`, code `0x889800B0`, visible dans le journal Application). Points notables de ces exigences : pour les formats, `SHADER_SAMPLE` veut dire « lisible par un shader » (les formats entiers et le stencil l'ont aussi) ; `R32G32B32_*` doivent accepter les textures 3D ; `B8G8R8X8_UNORM` et `R11G11B10_FLOAT` doivent servir de format de sommets ; `MAX_TEXTURE_REPEAT` (devcap 22) est exigé ; l'interface DirectX 11.1 demande en plus `YUY2` et `NV12`. La table de rsvga (`devcaps.rs`) suit maintenant ce que rapporte un vrai GPU Direct3D 11, et un test vérifie qu'elle couvre toutes les exigences du niveau 10_1.
- **Ce que le pilote noyau vérifie** : sa version *debug* (livrée avec les Tools, choisie par `guestinfo.svga.wddm.buildType=debug`) refuse par assertion les combinaisons impossibles (formats typeless ou de profondeur comme cible couleur, `R9G9B9E5` rendable...). Il refuse aussi de créer une surface dont le format n'est pas « supporté » : DWM utilise `ALPHA8` (le nom Direct3D 9 de `A8_UNORM`) pour son atlas de glyphes, et le pilote utilise `A8R8G8B8` pour `B8G8R8A8_UNORM` ; ces anciens noms ont donc les capacités de leur équivalent DXGI.
- **`DX_PRESENTBLT`** est implémenté (copie, mise à l'échelle au plus proche, inversion rouge/bleu RGBA↔BGRA, affichage immédiat si un Screen Target montre la destination) : c'est ainsi que le pilote présente, DirectX ou pas.
- **Niveau obtenu** : feature level 10_1 (SM 4.1). Annoncer le SM5 (devcap 258, `CAP2_DX3`) ouvre le 11_0 côté noyau ; il faudra alors aussi les shaders de calcul, de tessellation et les UAV (M4).
- **Direct3D 9** reste indisponible (`D3DERR_NOTAVAILABLE`) : son pilote utilisateur a ses propres exigences, sans doute les anciennes capacités `SURFACEFMT_*`. À étudier de la même façon.

### Outils de diagnostic

- `guest/d3dprobe/d3dprobe.c` (construit par `scripts/build-guest-tools.sh`, mingw-w64) : énumère les adaptateurs DXGI, crée des périphériques D3D11 à chaque niveau et avec plusieurs options, teste les formats, efface une cible et la relit, interroge D3D9. Il envoie chaque ligne à l'hôte par la porte dérobée VMware (`log d3dprobe: ...`, dans `qemu.log`) : il marche même quand l'invité n'affiche rien. `run-windows.sh --guest-tools` donne `target/guest-tools` à l'invité comme disque FAT en lecture seule.
- **Lire le disque de l'invité sans le démarrer** (VM arrêtée) : `qemu-storage-daemon` exporte la partition NTFS de la couche qcow2 en fichier par FUSE, sans droits root (`driver=raw` avec `offset`/`size` au-dessus du qcow2), et `7z` en extrait des fichiers : journaux d'événements (`Windows/System32/winevt/Logs`), rapports de plantage (`ProgramData/Microsoft/Windows/WER`).
- `--guestinfo` règle les journaux du pilote : `guestinfo.svga.wddm.miniportLogging=TRUE` (journal du pilote noyau), `guestinfo.loglevel.vm3d.<composant>=<niveau>`.

### Plan

1. **Décodage des commandes DX** (fait) : `svga-core/src/dx.rs` décode 90 commandes DX : celles de DirectX 10/10.1 (contextes, tables d'objets, états, vues, shaders, liaisons, dessins, copies, requêtes, fences, transferts), plus les variantes `_V2`, `GROW_COTABLE` et les copies de résolution et de conversion. Chaque champ est typé, chaque tableau borné par les limites de Direct3D (8 cibles, 32 tampons de sommets, 128 vues...), les énumérations fermées vérifiées ; tests et cible de fuzzing (`fuzz/run.sh dx`). Le serveur décode et trace (`DX <contexte> <commande>`) sans exécuter encore. Vérifié avec DWM : 26 701 commandes en une minute de bureau, sur 6 contextes, aucune refusée ni inconnue. Les commandes du shader model 5 (UAV, calcul, dessins indirects) viendront avec M4.
2. **État des contextes DX** (fait) : `svga-core/src/cotable.rs` donne le format exact de chaque entrée de table d'objets (tailles vérifiées en compilant les en-têtes VMware) : encodage et décodage, l'entrée passant par l'analyseur de sa commande `DEFINE`. `rsvga-server/src/dxctx.rs` garde les contextes (au plus 256), leurs douze tables liées à un MOB (un identifiant d'objet doit tenir dans la table : la taille du MOB borne la mémoire), les objets, l'état du pipeline (shaders, vues, tampons, états, fenêtres de rendu, ciseaux, stream output, prédication) et le code des shaders, copié hors du MOB au `BIND_SHADER` (64 Mio en tout, 4 Mio par shader). Le pilote Windows agrandit ses tables par `DX_GROW_COTABLE` : les objets suivent. `DX_SET_COTABLE` avec contenu valide recharge une table depuis son MOB, `DX_READBACK_COTABLE` l'y écrit. Les fences 64 bits et les transferts MOB ↔ surface sont exécutés ; les requêtes se terminent tout de suite avec des résultats « visible, rien de compté » en attendant le GPU. Sauvegarder ou restaurer l'état complet d'un contexte (`READBACK_CONTEXT`, `BIND_CONTEXT` avec contenu valide) n'est pas fait : Windows ne s'en sert pas. Vérifié avec DWM : 28 265 commandes, 25 176 exécutées, 3 089 en attente du rendu (dessins, copies, effacements), aucune refusée.
3. **`svga-render`** (Vulkan via `ash` 0.38, sur la RX 6950 XT) — **fondations faites** : le GPU est le plus gros GPU dédié (ou celui que nomme `RSVGA_GPU`, par exemple `llvmpipe`). Une surface devient une `VkImage` ou un `VkBuffer` dont les octets ont la disposition de sa copie invitée : les formats ne correspondent qu'à des formats Vulkan de même disposition, et l'envoi comme la relecture passent sans conversion par des tampons intermédiaires. Toutes les images restent en layout `GENERAL`, les opérations sont séparées par des barrières complètes, le travail est soumis à la fin de chaque command buffer de l'invité : simple et correct, à affiner avec les dessins. `rsvga-server/src/gpu.rs` garde chaque surface cohérente : il retient quel côté (copie hôte ou GPU) a été écrit en dernier et copie avant que l'autre ne serve (relecture vers l'invité, affichage, copie CPU, envoi depuis le MOB). Exécutés sur le GPU : `CLEAR_RENDERTARGET_VIEW`, `PRED_COPY(_REGION)`, `BUFFER_COPY`, `TRANSFER_FROM_BUFFER`, `SURFACE_COPY_AND_READBACK`. **Premier rendu vérifié** : sous Windows, `d3dprobe` efface une cible en rouge, la copie et relit `0xffff0000`, le tout exécuté par la RX 6950 XT. Restent : vues, objets d'état, pipelines (rendu dynamique), dessins ; les formats de profondeur 24 bits (stockés en 32 bits flottants sur AMD) et les surfaces multi-échantillonnées ne font pas encore l'aller-retour.
4. **Traducteur de shaders VGPU10 → SPIR-V** : bytecode SM4.1 (VS et PS d'abord, GS ensuite), validé par `spirv-val`.
5. **Présentation** : Screen Targets alimentés depuis le GPU (copie vers la fenêtre d'abord, dmabuf plus tard).
6. **Critères** : le bureau de DWM s'affiche correctement en 3D ; `d3dprobe` relit du rouge ; un triangle D3D11 (ajouté à `guest/`) ; `dxdiag` sans erreur. Alors seulement, la 3D sera annoncée par défaut.

## Questions ouvertes

- **Réglages `guestinfo.svga.wddm.*`** : le patch 0002 sait maintenant les servir, mais rsvga n'en fixe aucun ; le pilote prend ses défauts. Reste à savoir si certains méritent une valeur (VMware les prend dans le `.vmx`).
- **Curseur par MOB, ScreenDMA, IntraSurfaceCopy** (`CAP2_CURSOR_MOB`, `SCREENDMA_REG`, `INTRA_SURFACE_COPY`) : non annoncés pour l'instant. Le pilote garde le curseur par la FIFO, qui marche ; il écrit « cannot determine ScreenDMA support » sans conséquence visible en mode Display Only.
- **Plusieurs écrans** : les Tools annoncent `display_topology_set 2`. `DisplayTopology_Set` permettrait plusieurs fenêtres-écrans ; rsvga n'en montre qu'un pour l'instant.
- **Patchs QEMU à proposer upstream** : 0001 (accès non alignés vfio-user) et 0002 (port VMware), une fois le design validé par l'usage.
- **Affichage** : vfio-user ne transporte pas l'image vers la fenêtre de QEMU (`hw/vfio/display.c` appelle `ioctl` directement sur le fd du périphérique). En attendant, le serveur ouvre sa propre fenêtre et envoie clavier et souris par QMP. Cible : étendre le protocole vfio-user (GFX plane + dmabuf).
- **KVM `enable_vmware_backdoor=Y`** : activé sur cette machine pour les VMware Tools. Le paramètre est global au module `kvm` et ne change qu'au rechargement du module (toutes les VM arrêtées) ; le noyau 7.4 n'a pas de réglage par VM. La commande 75 vient du noyau de l'invité et passe sans ce réglage.
- **Shader model 5** : quelles capacités supplémentaires (devcaps, `CAP2_DX3`...) le pilote attend pour le niveau 11_0, et à quel moment les annoncer (M4).
- **VMware Tools** (pilote invité) : logiciel propriétaire, téléchargé par l'utilisateur, jamais redistribué par ce projet.
- **Licence du projet** : MIT OR Apache-2.0 proposée, à confirmer.

## Références

- VMware SVGA Device Developer Kit : en-têtes `svga_reg.h`, `svga3d_*.h` (licence MIT).
- Pilote Linux `vmwgfx` et pilote Mesa `svga` : références de ce que fait un vrai pilote invité.
- QEMU `hw/vfio-user/` et la spécification `docs/interop/vfio-user.rst`.
- DXVK (zlib) : référence de traduction D3D11 → Vulkan. On s'inspire des idées, sans copier de code.
- **On ne copie pas de code de qemu-vmvga** (GPL, généré par IA) : tout est réimplémenté proprement à partir de la spécification.

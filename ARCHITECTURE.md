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
| `svga-core` | Logique pure du périphérique : FIFO, décodage des commandes, registres, objets. Sans I/O et sans `unsafe`. | M0 → M2 |
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
| **M1 : bureau 2D** (en cours) | Serveur vfio-user, registres SVGA, VRAM et FIFO en mémoire partagée, mode graphique, `UPDATE` affiché dans une fenêtre. | OVMF puis Windows affichés avec le pilote VMware, en 2D. |
| **M2 : pilote WDDM** | Objets « guest-backed » (MOB, OTables), command buffers, Screen Targets, curseur, fences et IRQ. | Bureau Windows 11 à la résolution native, redimensionnement. |
| **M3 : 3D de base** | Contexte DX, ressources, vues, clear/copy/draw, traducteur VGPU10 → SPIR-V (VS/PS). | Triangle D3D11, `dxdiag` sans erreur. |
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
- **Reste pour finir M1** : installer le pilote VMware SVGA dans Windows et implémenter ce qu'il demande en 2D (GMR, screen objects, curseur), puis faire passer clavier et souris depuis la fenêtre de rsvga (QMP).

## Questions ouvertes

- **Commande 75 du port VMware** : le pilote user-mode lit les capacités SVGA par le port « backdoor ». Le QEMU officiel ne gère pas cette commande, il faudra un petit patch QEMU, à proposer upstream sous forme générique.
- **Affichage** : vfio-user ne transporte pas l'image vers la fenêtre de QEMU (`hw/vfio/display.c` appelle `ioctl` directement sur le fd du périphérique). En attendant, le serveur ouvre sa propre fenêtre et envoie clavier et souris par QMP. Cible : étendre le protocole vfio-user (GFX plane + dmabuf).
- **KVM `enable_vmware_backdoor=Y`** : nécessaire pour que le pilote user-mode accède au port VMware.
- **Windows 11 limité aux feature levels 9_x** avec qemu-vmvga : comprendre quelles capacités DX, SM4.1 et SM5 le pilote attend.
- **VMware Tools** (pilote invité) : logiciel propriétaire, téléchargé par l'utilisateur, jamais redistribué par ce projet.
- **Licence du projet** : MIT OR Apache-2.0 proposée, à confirmer.

## Références

- VMware SVGA Device Developer Kit : en-têtes `svga_reg.h`, `svga3d_*.h` (licence MIT).
- Pilote Linux `vmwgfx` et pilote Mesa `svga` : références de ce que fait un vrai pilote invité.
- QEMU `hw/vfio-user/` et la spécification `docs/interop/vfio-user.rst`.
- DXVK (zlib) : référence de traduction D3D11 → Vulkan. On s'inspire des idées, sans copier de code.
- **On ne copie pas de code de qemu-vmvga** (GPL, généré par IA) : tout est réimplémenté proprement à partir de la spécification.

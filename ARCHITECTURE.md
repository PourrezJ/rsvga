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
| `rsvga-server` | Binaire : serveur vfio-user, mmap de la mémoire invitée, IRQ, sandbox, fenêtre. | M1 |

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
| **M1 : bureau 2D** | Serveur vfio-user, registres SVGA, VRAM et FIFO en mémoire partagée, mode graphique, `UPDATE` affiché dans une fenêtre. | OVMF puis Windows affichés avec le pilote VMware, en 2D. |
| **M2 : pilote WDDM** | Objets « guest-backed » (MOB, OTables), command buffers, Screen Targets, curseur, fences et IRQ. | Bureau Windows 11 à la résolution native, redimensionnement. |
| **M3 : 3D de base** | Contexte DX, ressources, vues, clear/copy/draw, traducteur VGPU10 → SPIR-V (VS/PS). | Triangle D3D11, `dxdiag` sans erreur. |
| **M4 : D3D11 complet** | GS/HS/DS/CS, stream output, requêtes, MSAA, capacités FL 11_0. | 3DMark et jeux DX11. |
| **M5 : intégration** | Sandbox, patchs QEMU proposés upstream, paquet Debian. | Installation en une commande. |

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

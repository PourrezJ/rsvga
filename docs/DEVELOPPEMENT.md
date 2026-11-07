# Développement

Ce qui sert à travailler sur rsvga : tests, fuzzing, lancement à la main, outils de test dans Windows. La conception est dans [ARCHITECTURE.md](../ARCHITECTURE.md).

## Organisation du code

- `crates/svga-proto` : registres, FIFO, capacités et identifiants de commandes du protocole VMware SVGA II (constantes seulement).
- `crates/svga-core` : FIFO validée, décodage des commandes 2D, 3D anciennes (Direct3D 9) et DX, registres SVGA. Rust sûr, borné, protégé contre un invité qui modifierait une commande pendant sa lecture.
- `crates/vfio-user` : protocole vfio-user côté serveur, avec passage de descripteurs de fichiers.
- `crates/svga-shader` : traduction des shaders Direct3D 9 (shader models 1 à 3) et Direct3D 10 (VGPU10, SM 4.0 et 4.1) en SPIR-V.
- `crates/svga-render` : moteur Vulkan (`ash`) : surfaces de l'invité sur le GPU de l'hôte, dessins, effacements, copies, relectures sans attente.
- `crates/rsvga-server` : la carte complète (config PCI, VRAM et FIFO en memfd scellés, ROM, exécution des commandes, capture PNG, fenêtre optionnelle).
- `firmware/gop` : pilote UEFI GOP en Rust `no_std` (7 Ko), livré dans la ROM de la carte.
- `guest/` : programmes de test côté Windows (C, mingw-w64).
- `patches/qemu` : les deux patchs de QEMU.

## Tests et fuzzing

```bash
cargo test --workspace
```

186 tests, dont un test de stress sur 20 000 FIFO aléatoires et une session vfio-user de bout en bout. Sans GPU Vulkan, les tests du moteur se sautent eux-mêmes.

Fuzzing avec libFuzzer, sur la toolchain stable (ni cargo-fuzz ni nightly). Cibles : `fifo`, `decode`, `dx`, `shader`, `nine`.

```bash
fuzz/run.sh dx 60
```

## QEMU patché

- [0001](../patches/qemu/0001-vfio-user-pass-unaligned-region-accesses-through.patch) : les pilotes VMware parlent à la carte par des accès non alignés, que QEMU refuse sur les régions vfio. Le patch les laisse passer, pour vfio-user seulement.
- [0002](../patches/qemu/0002-vmport-answer-SVGA-capability-queries-and-GuestRPC.patch) : le pilote Windows demande les capacités de la carte à l'hyperviseur par le port VMware (commande 75), et y écrit son journal (GuestRPC). Le patch ajoute les deux, désactivés par défaut ; `run-windows.sh` les active avec les valeurs que donne `rsvga --vmport-args`.

`scripts/build-qemu.sh` construit QEMU 11.1.2 patché dans `target/qemu/install`, sans toucher au QEMU du système. Dépendances sur Debian :

```bash
sudo apt install libpixman-1-dev libslirp-dev libgtk-3-dev libfdt-dev
```

Tests de bout en bout (rsvga, QEMU et OVMF) : registres pilotés comme par un vrai pilote, puis console UEFI affichée par le pilote de la carte :

```bash
scripts/smoke-qemu.py
```

```bash
scripts/smoke-qemu.py --firmware
```

## Lancer la carte à la main

`scripts/run-windows.sh` fait tout ça ; voici le détail.

```bash
cargo build --release -p rsvga-server --features window
```

```bash
target/release/rsvga --socket $XDG_RUNTIME_DIR/rsvga.sock --rom target/firmware/rsvga-gop.efi --window --qmp $XDG_RUNTIME_DIR/qemu-qmp.sock
```

Côté QEMU, la RAM de l'invité doit être partagée (memfd), et la carte s'ajoute ainsi :

```text
-object memory-backend-memfd,id=mem,size=4G,share=on -machine memory-backend=mem
-device '{"driver":"vfio-user-pci","socket":{"type":"unix","path":"/run/user/1000/rsvga.sock"}}'
```

`--window` affiche l'écran de l'invité, et `--qmp CHEMIN` transmet clavier et souris à QEMU par son socket QMP (`-qmp unix:CHEMIN,server=on,wait=off` côté QEMU). Les touches sont envoyées par position physique : un clavier AZERTY sur l'hôte et dans Windows s'accordent touche pour touche. La souris passe en absolu, par une tablette USB (`-device qemu-xhci -device usb-tablet`). Sans fenêtre, `--snapshot FICHIER.png` enregistre l'écran.

## Variables d'environnement

| Variable | Effet |
|---|---|
| `RSVGA_3D=0` | Garde la carte en 2D (la 3D est annoncée par défaut quand un GPU Vulkan est là). |
| `RSVGA_GPU=<partie du nom>` | Choisit le GPU (par défaut le plus gros GPU dédié). |
| `RSVGA_TRACE=1` | Journalise chaque accès aux registres, chaque commande et l'état de la FIFO toutes les 5 secondes ; `2` ajoute les accès bruts ; `0` rend le serveur silencieux. |
| `RSVGA_PROFILE=1` | Journalise où va le temps, GPU compris. |
| `RSVGA_SYNC_BUFFERS=1` | Exécute chaque command buffer pendant l'écriture de registre de l'invité, sans fil de travail (diagnostic). |

## Outils de test dans Windows

Construits avec mingw-w64 et donnés à la VM par `run-windows.sh --guest-tools` (lecteur `D:`). Ils rapportent à l'hôte par la porte dérobée VMware (`qemu.log`), même quand l'invité n'affiche rien.

```bash
scripts/build-guest-tools.sh
```

- `d3dtest` : vérifications de rendu Direct3D 11, processeur vidéo compris.
- `d3d9test` : la même chose en Direct3D 9.
- `d3dprobe` : ce que Direct3D voit de la carte.
- `umdcaps` : ce que le pilote D3D9 répondrait au runtime ; `d3d9trace` : ce que le runtime lui demande vraiment.
- `videoprobe` : blits du processeur vidéo, un paramètre à la fois.
- `modes` : changements de résolution.
- `hostcat` (envoie un fichier texte, par exemple le rapport de `dxdiag`), `wininfo` (fenêtres visibles).

Avec la VM lancée (`--diagnose --guest-tools`), `scripts/guest-run.sh` tape la commande dans Windows et affiche le résultat :

```bash
scripts/guest-run.sh run
```

Commandes : `run` (d3dtest), `ninetest` (d3d9test), `diag` (dxdiag), `probe`, `info`, `events` (dernières erreurs), `umd`, `trace`, `nine` (d3dprobe avec le journal du pilote D3D9 de débogage), `modes`, `videoprobe`, `reboot`, `heaven` puis `heavenrun` (Unigine Heaven en Direct3D 9, si son installateur est dans `target/unigine/download`), et en `--admin`, `dxg` et `events` (canaux du noyau graphique).

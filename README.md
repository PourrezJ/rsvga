# rsvga

Carte graphique virtuelle VMware SVGA II, écrite en Rust, pour accélérer DirectX 9 à 11 dans les VM Windows sous QEMU/KVM, sans passthrough GPU.

- Windows utilise le pilote VMware SVGA officiel, déjà signé.
- rsvga tourne **à côté** de QEMU, dans un processus sandboxé, et s'y branche par vfio-user.
- Le rendu passe directement par Vulkan sur le GPU de l'hôte.

Statut : **M1 en cours**. Avec le QEMU patché (une petite modification de vfio-user), le firmware UEFI puis Windows 11 s'affichent sur rsvga grâce à son propre pilote UEFI ; le pilote VMware de Windows reste à brancher. Le détail est dans [ARCHITECTURE.md](ARCHITECTURE.md).

![Console UEFI affichée par rsvga](docs/img/m1-ovmf-shell.png)

## Compiler et tester

```bash
cargo test --workspace
```

Fuzzing avec libFuzzer, sur la toolchain stable (ni cargo-fuzz ni nightly) :

```bash
fuzz/run.sh fifo 60
```

```bash
fuzz/run.sh decode 60
```

## QEMU patché

Les pilotes VMware parlent à la carte par des accès non alignés, que QEMU refuse sur les régions vfio. Le patch [patches/qemu/0001-vfio-user-pass-unaligned-region-accesses-through.patch](patches/qemu/0001-vfio-user-pass-unaligned-region-accesses-through.patch) corrige ça pour vfio-user seulement. Pour construire QEMU 11.1.2 patché dans `target/qemu/install`, sans toucher au QEMU du système :

```bash
sudo apt install libpixman-1-dev libslirp-dev libgtk-3-dev libfdt-dev
```

```bash
scripts/build-qemu.sh
```

Pilote UEFI de la carte (servi depuis sa ROM, voir `--rom`) :

```bash
scripts/build-firmware.sh
```

Tests de bout en bout (rsvga + QEMU + OVMF) : registres pilotés comme par un vrai pilote, puis console UEFI affichée par notre pilote :

```bash
scripts/smoke-qemu.py
```

```bash
scripts/smoke-qemu.py --firmware
```

## Lancer la carte

```bash
cargo build --release -p rsvga-server
```

```bash
target/release/rsvga --socket $XDG_RUNTIME_DIR/rsvga.sock --rom target/firmware/rsvga-gop.efi --snapshot /tmp/rsvga.png
```

Côté QEMU, la RAM du guest doit être partagée (memfd), et la carte s'ajoute ainsi :

```text
-object memory-backend-memfd,id=mem,size=4G,share=on -machine memory-backend=mem
-device '{"driver":"vfio-user-pci","socket":{"type":"unix","path":"/run/user/1000/rsvga.sock"}}'
```

`--window` ouvre une fenêtre (compiler avec `--features window`). `RSVGA_TRACE=1` journalise chaque accès aux registres et à l'espace PCI.

## Ce qui existe

- `svga-proto` : registres, FIFO, capacités et identifiants de commandes du protocole VMware SVGA II.
- `svga-core` : FIFO validée, décodage de toutes les commandes 2D et de l'encadrement 3D, registres SVGA (modes, ID, palette...). Rust sûr, borné, protégé contre un invité qui modifierait une commande pendant sa lecture.
- `vfio-user` : protocole vfio-user côté serveur, avec passage de descripteurs de fichiers.
- `rsvga-server` : la carte complète (config PCI, VRAM et FIFO en memfd scellés, ROM optionnelle, exécution de la FIFO, capture PNG, fenêtre optionnelle).
- `firmware/gop` : pilote UEFI GOP en Rust `no_std` (7 Ko), livré dans la ROM de la carte.
- 60 tests, dont un test de stress sur 20 000 FIFO aléatoires et une session vfio-user de bout en bout ; deux cibles de fuzzing.

L'audit de qemu-vmvga qui a motivé ce projet est dans [docs/audit-qemu-vmvga.md](docs/audit-qemu-vmvga.md).

## Licence

MIT OR Apache-2.0 (proposition). Les constantes de `svga-proto` sont transcrites des en-têtes VMware sous licence MIT : voir `crates/svga-proto/VMWARE-NOTICE.txt`.

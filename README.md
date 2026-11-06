# rsvga

Carte graphique virtuelle VMware SVGA II, écrite en Rust, pour accélérer DirectX 9 à 11 dans les VM Windows sous QEMU/KVM, sans passthrough GPU.

- Windows utilise le pilote VMware SVGA officiel, déjà signé.
- rsvga tourne **à côté** de QEMU, dans un processus sandboxé, et s'y branche par vfio-user.
- Le rendu passe directement par Vulkan sur le GPU de l'hôte.

Statut : **M0, fondations** (terminé). Rien n'est encore utilisable dans une VM. Le détail est dans [ARCHITECTURE.md](ARCHITECTURE.md).

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

## Ce qui existe (M0)

- `svga-proto` : registres, FIFO, capacités et identifiants de commandes du protocole VMware SVGA II.
- `svga-core` : lecture de la FIFO partagée avec l'invité et décodage de toutes les commandes 2D, plus l'encadrement des commandes 3D. Tout est en Rust sûr, borné et protégé contre un invité qui modifierait une commande pendant sa lecture.
- 30 tests, dont un test de stress sur 20 000 FIFO aléatoires, et deux cibles de fuzzing.

L'audit de qemu-vmvga qui a motivé ce projet est dans [docs/audit-qemu-vmvga.md](docs/audit-qemu-vmvga.md).

## Licence

MIT OR Apache-2.0 (proposition). Les constantes de `svga-proto` sont transcrites des en-têtes VMware sous licence MIT : voir `crates/svga-proto/VMWARE-NOTICE.txt`.

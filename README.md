# rsvga

Carte graphique virtuelle **VMware SVGA II** écrite en Rust. Elle donne Direct3D 9, 10 et 11 aux VM Windows sous QEMU/KVM, rendus par Vulkan sur le GPU de l'hôte, sans céder ce GPU à la VM.

- Windows utilise le pilote VMware SVGA officiel, déjà signé : ni mode test, ni pilote maison.
- rsvga tourne dans son propre processus, branché à QEMU par vfio-user.
- Les commandes 3D du pilote sont décodées, leurs shaders traduits en SPIR-V, puis exécutés par Vulkan.

## État

Projet expérimental. Testé avec Windows 11 et le pilote `vm3d` 9.17.11 (VMware Tools 13.1.5) :

- bureau composé par le GPU, résolution qui suit la fenêtre, curseur matériel ;
- Direct3D 11 au niveau 11_0 (WDDM 2.0) : shader model 5, compute shaders, tessellation, geometry shaders, stream output, UA views ; Direct3D 9 (shader model 3) ;
- Unigine Heaven 4.0 en 1280×720 sur une RX 6950 XT : en Direct3D 9, 35 à 38 images par seconde ; en Direct3D 11 avec la tessellation, 40 en moyenne (31 à 55 selon les scènes).

Pas encore : les compteurs des tampons append/consume, `switch` et les sous-routines des shaders, les doubles, le niveau 11_1. Le pilote de VMware ne fait ni Direct3D 12 ni Vulkan.

## Prérequis

- Linux avec KVM, et un GPU Vulkan 1.3.
- Rust 1.87 ou plus récent.
- Les paquets de compilation de QEMU (sur Debian : `libpixman-1-dev libslirp-dev libgtk-3-dev libfdt-dev`), OVMF et `qemu-img`.
- Une VM Windows 11 en UEFI, et les VMware Tools, à télécharger soi-même : ce projet ne les distribue pas.

## Démarrer

Construire QEMU avec les deux patchs de `patches/qemu` (dans `target/`, sans toucher au QEMU du système), le pilote UEFI de la carte, puis rsvga :

```bash
scripts/build-qemu.sh
```

```bash
scripts/build-firmware.sh
```

```bash
cargo build --release -p rsvga-server --features window
```

Lancer la VM dans la fenêtre de rsvga. Le disque passe par une couche qcow2 temporaire : l'image d'origine n'est jamais modifiée.

```bash
scripts/run-windows.sh --disk windows.qcow2 --vars OVMF_VARS_4M.fd
```

Dans Windows, installer les VMware Tools, ou seulement le pilote (`pnputil /add-driver vm3d.inf /install`). Pour que les Tools atteignent le port VMware, l'hôte doit charger KVM avec `enable_vmware_backdoor=Y`.

`--no-3d` (ou `RSVGA_3D=0`) garde la carte en 2D. `RSVGA_GPU=<partie du nom>` choisit le GPU.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) : conception, sécurité, et ce que chaque étape a appris du pilote.
- [docs/DEVELOPPEMENT.md](docs/DEVELOPPEMENT.md) : organisation du code, tests, fuzzing, options, outils de test dans Windows.

## Projet indépendant

rsvga n'est ni affilié à VMware ou Broadcom, ni approuvé par eux. VMware est une marque de Broadcom Inc. Ce dépôt ne contient aucun logiciel de VMware : le pilote vient des VMware Tools, que chacun télécharge et utilise selon leur licence.

## Licence

MIT, voir [LICENSE](LICENSE). Deux exceptions :

- les constantes du protocole (`crates/svga-proto`) viennent des en-têtes de VMware, eux aussi sous licence MIT : voir [VMWARE-NOTICE.txt](crates/svga-proto/VMWARE-NOTICE.txt) ;
- les patchs de `patches/qemu` modifient QEMU et suivent sa licence (GPL v2).

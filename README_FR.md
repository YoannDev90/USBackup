# USBackup 🚀

Un agent de sauvegarde automatique pour périphériques USB écrit en Rust. Ce programme tourne en arrière-plan (24h/24) et détecte instantanément le branchement de clés USB pour déclencher des actions prédéfinies.

[English version here](README.md)

## ✨ Fonctionnalités

- **Surveillance en temps réel** : Détecte les événements de branchement/débranchement sans polling (utilisation des APIs natives via `nusb`).
- **Interface TUI Moderne** : Une interface élégante dans le terminal utilisant `ratatui` pour suivre l'activité en direct.
- **Auto-montage Intelligent** : Trouve et monte automatiquement les partitions USB via `udev` et `udisksctl`.
- **Whitelisting interactif** : Lorsqu'un nouvel appareil est détecté, l'application vous demande s'il faut le mémoriser, l'ignorer ou poser la question plus tard.
- **Architecture Multi-thread** : L'interface reste fluide même pendant les sauvegardes lourdes en arrière-plan.
- **Notifications Système** : Utilise les notifications natives du bureau pour vous tenir informé.

## 🛠️ Installation

### Dépendances (Linux)
Vous devez avoir les fichiers de développement `libudev` installés :
- **Fedora/RHEL** : `sudo dnf install libudev-devel`
- **Ubuntu/Debian** : `sudo apt install libudev-dev pkg-config`

### Compilation
1. Assurez-vous d'avoir [Rust](https://www.rust-lang.org/) installé.
2. Clonez le dépôt.
3. Compilez et lancez :
   ```bash
   cargo run
   ```

## ⚙️ Configuration

Le fichier `backup_config.json` gère vos périphériques connus. Voici la structure typique :

```json
{
  "devices": {
    "0781:5581": {
      "name": "Ma Clé SanDisk",
      "vendor_id": 1921,
      "product_id": 21889,
      "action": "Whitelist",
      "backup_rules": [
        {
          "source_path": "/chemin/vers/usb/data",
          "destination_path": "/home/user/backups/sandisk/",
          "exclude": [".tmp", "cache/"]
        }
      ]
    }
  }
}
```

### Actions disponibles :
- `Whitelist` : Déclenche automatiquement les sauvegardes.
- `IgnoreForever` : Ne pose plus de questions et ignore l'appareil.
- `AskEachTime` : Redemande l'action à chaque branchement.

## 🚀 Prochaines étapes

- [x] Implémenter le montage automatique des partitions (udev/udisksctl).
- [x] Interface TUI avec logs temps réel.
- [ ] Ajouter la synchronisation via `rsync` ou logique Rust native.
- [x] Notification système lors de la fin d'un backup.

## ⚖️ Licence

MIT

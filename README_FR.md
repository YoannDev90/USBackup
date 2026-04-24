# USBackup 🚀

Un agent de sauvegarde automatique pour périphériques USB écrit en Rust. Ce programme tourne en arrière-plan (24h/24) et détecte instantanément le branchement de clés USB pour déclencher des actions prédéfinies.

[English version here](README.md)

## ✨ Fonctionnalités

- **Surveillance en temps réel** : Détecte les événements de branchement/débranchement sans polling (utilisation des APIs natives via `nusb`).
- **Whitelisting interactif** : Lorsqu'un nouvel appareil est détecté, l'application vous demande s'il faut le mémoriser, l'ignorer ou poser la question plus tard.
- **Configuration flexible** : Gérez des règles de sauvegarde spécifiques (sources, destinations, exclusions) pour chaque périphérique.
- **Logs colorés** : Suivi clair de l'activité directement dans le terminal.

## 🛠️ Installation

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

- [ ] Implémenter le montage automatique des partitions (Linux/Udisksctl).
- [ ] Ajouter la synchronisation via `rsync`.
- [ ] Notification système lors de la fin d'un backup.

## ⚖️ Licence

MIT

# USBackup 🚀

Un agent de sauvegarde automatique pour périphériques USB écrit en Rust. Ce programme tourne en arrière-plan (24h/24) et détecte instantanément le branchement de clés USB pour déclencher des actions prédéfinies.

[English version here](README.md)

## ✨ Fonctionnalités

- **Détection temps-réel** : Détecte les branchements/débranchements sans polling (via `nusb`).
- **Standards XDG** : Configuration et logs suivent les standards Linux (`~/.config/usbackup` et `~/.local/share/usbackup`).
- **Snapshots Incrémentaux** : Support du versionnage par date via les *hard links* rsync (très économe en espace).
- **Actions Post-Sauvegarde** : Démontage automatique ou exécution de scripts personnalisés.
- **Configuration Décentralisée** : Les paramètres sont stockés directement sur les périphériques USB (`.usbackup.toml`).
- **Sécurité HMAC** : Signatures cryptographiques pour empêcher toute exécution non autorisée.
- **Auto-montage Intelligent** : Monte automatiquement les partitions USB via `udev` et `udisksctl`.
## 🛠️ Comment ça marche (Technique)

USBackup utilise un modèle de **Configuration Décentralisée** avec une approche sécurisée **Zero-Trust** :

1. **Détection** : Écoute les événements `udev` via `nusb` pour une détection instantanée (pas de polling).
2. **Identification** : Utilise l'**UUID** de partition pour distinguer les différents périphériques.
3. **Signature HMAC** :
   - Une clé secrète (`secret_key`) unique est générée sur votre machine.
   - Chaque config sur clé (`.usbackup.toml`) est signée avec ce secret via **HMAC-SHA256**.
   - Cela empêche un utilisateur malveillant d'injecter sa propre configuration pour voler des fichiers.
4. **Exécution** :
   - **Mode Miroir** : Synchronisation simple.
   - **Snapshots Incrémentaux** : Utilise `rsync --link-dest` pour créer des versions datées sans doubler l'espace disque.
   - **Post-Sauvegarde** : Démontage automatique ou script custom.
   - **Exclusions Intelligentes** : Respecte les règles `.gitignore` locales.
## � OS Supportés

| OS          | Statut         | Notes                                                               |
| :---------- | :------------- | :------------------------------------------------------------------ |
| **Linux**   | ✅ Supporté     | Support natif de udev et auto-montage.                              |
| **Windows** | ❌ Non Supporté | Si vous voulez le support Windows, merci de faire un **Fork + PR**. |
| **macOS**   | ❌ Non Supporté | Si vous voulez le support macOS, merci de faire un **Fork + PR**.   |

## �🛠️ Installation

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

USBackup utilise un modèle de configuration décentralisé.

1. **Config Locale** : Dans `~/.config/usbackup/backup_config.toml`, contient les UUID approuvés et votre **clé secrète**.
2. **Logs** : Les logs détaillés sont dans `~/.local/share/usbackup/logs/`.
3. **`.usbackup.toml`** (Périphérique) : À la racine de votre clé USB.

### Exemple `.usbackup.toml` :

```toml
name = "Ma Clé SanDisk"
vendor_id = 1921
product_id = 21889
uuid = "1234-ABCD"
signature = "a1b2c3d4..."
action = "Whitelist"

[[backup_rules]]
source_path = "/home/user/Documents"
destination_path = "backups/docs"
exclude = [".tmp", "cache/"]
incremental = true           # Active les snapshots
unmount_after = true        # Démontage auto
post_backup_script = "notify-send 'Backup terminée !'"
```

### Actions disponibles :
- `Whitelist` : Déclenche automatiquement les sauvegardes.
- `IgnoreForever` : Ne pose plus de questions et ignore l'appareil.
- `AskEachTime` : Redemande l'action à chaque branchement.

## 🚀 Prochaines étapes

- [x] Configuration décentralisée TOML.
- [x] Signature HMAC pour la sécurité.
- [x] Montage automatique des partitions.
- [x] Standards XDG (chemins config/logs).
- [x] Snapshots incrémentaux (versionnage).
- [x] Actions post-sauvegarde (script/démontage).
- [x] Notifications système.
- [ ] Sauvegardes multi-cibles (SSH distant/Cloud).

## ⚖️ Licence

MIT

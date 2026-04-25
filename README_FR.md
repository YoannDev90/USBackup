# USBackup 🚀

Un agent de sauvegarde automatique pour périphériques USB écrit en Rust. Ce programme tourne en arrière-plan (24h/24) et détecte instantanément le branchement de clés USB pour déclencher des actions prédéfinies.

[English version here](README.md)

## ✨ Fonctionnalités

- **Configuration Décentralisée** : Les paramètres sont stockés directement sur les périphériques USB (`.usbackup.toml`), ce qui les rend portables.
- **Sécurité HMAC** : Les configurations sont signées cryptographiquement avec une clé secrète locale pour empêcher toute exécution non autorisée.
- **Plusieurs Formats de Sauvegarde** : Choisissez entre la synchronisation standard (`rsync`), des archives ZIP ou des archives TarGz.
- **Auto-montage Intelligent** : Trouve et monte automatiquement les partitions USB via `udev` et `udisksctl`.
- **Whitelisting interactif** : Lorsqu'un nouvel appareil est détecté, l'application vous demande s'il faut le mémoriser, l'ignorer ou poser la question plus tard.
- **Architecture Multi-thread** : L'agent reste réactif même pendant les sauvegardes lourdes.
- **Notifications Système** : Utilise les notifications natives du bureau pour vous tenir informé.
## 🛠️ Comment ça marche (Technique)

USBackup utilise un modèle de **Configuration Décentralisée** avec une approche sécurisée **Zero-Trust** :

1. **Détection** : Écoute les événements `udev` via `nusb` pour une détection instantanée (pas de polling).
2. **Identification** : Utilise l'**UUID** de partition pour distinguer les différents périphériques.
3. **Signature HMAC** :
   - Une clé secrète (`secret_key`) unique est générée sur votre machine.
   - Chaque config sur clé (`.usbackup.toml`) est signée avec ce secret via **HMAC-SHA256**.
   - Cela empêche un utilisateur malveillant d'injecter sa propre configuration pour voler des fichiers.
4. **Exécution** :
   - **Mode Miroir** : Synchronisation incrémentale via l'algorithme delta de `rsync`.
   - **Mode Archive** : Crée des fichiers horodatés `.zip` ou `.tar.gz` pour le versionnage.
   - **Exclusions Intelligentes** : Respecte automatiquement les règles de votre `.gitignore` local.
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

1. **`backup_config.toml`** (Local) : Stocké dans le dossier de l'application, il contient la liste des UUID approuvés et votre **clé secrète** machine pour la signature.
2. **`.usbackup.toml`** (Périphérique) : Stocké à la racine de votre clé USB. Il contient les règles de sauvegarde et la signature HMAC.

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
compression = "Zip" # Options: None, Zip, TarGz
```

### Actions disponibles :
- `Whitelist` : Déclenche automatiquement les sauvegardes.
- `IgnoreForever` : Ne pose plus de questions et ignore l'appareil.
- `AskEachTime` : Redemande l'action à chaque branchement.

## 🚀 Prochaines étapes

- [x] Configuration décentralisée TOML.
- [x] Signature HMAC pour la sécurité.
- [x] Support de la compression (ZIP, TarGz).
- [x] Montage automatique des partitions.
- [ ] Interface TUI moderne avec `ratatui`.
- [x] Notifications système.

## ⚖️ Licence

MIT

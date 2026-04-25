# Syntaxe des Patterns d'Exclusion - USBackup

USBackup utilise la syntaxe standard des fichiers `.gitignore`. Ces patterns sont passés directement à `rsync` pour exclure des fichiers ou répertoires.

## Caractères Standards

| Pattern | Description | Exemple |
|:---:|:---|:---|
| `*` | N'importe quelle suite de caractères (sauf `/`) | `*.tmp` |
| `**` | N'importe quel nombre de répertoires | `**/temp/**` |
| `?` | Un seul caractère exactement | `file?.txt` |
| `/` | Séparateur de répertoire | `logs/` |
| `!` | Négation (ne pas exclure ce pattern) | `!important.txt` |

## Presets Disponibles

L'assistant propose des modèles prédéfinis basés sur les standards de l'industrie :

- **Node.js** : `node_modules/`, logs de debug, etc.
- **Python** : `__pycache__`, environnements virtuels (`.venv`).
- **Rust** : Le dossier `target/`.
- **macOS / Windows** : Fichiers systèmes inutiles (`.DS_Store`, `Thumbs.db`).
- **Images/RAW** : Formats RAW volumineux (`*.raw`, `*.cr2`, `*.nef`).
- **Temporary** : Fichiers temporaires communs (`*.tmp`, `*.bak`).

## Note Technique

Les patterns sont convertis en options `--exclude` pour `rsync`. Ils s'appliquent de manière relative à la racine du dossier source sélectionné.

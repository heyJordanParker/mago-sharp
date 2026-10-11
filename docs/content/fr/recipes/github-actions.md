+++
title = "Recette GitHub Actions"
description = "Lancer le formatage, le linting et l'analyse à chaque push et chaque pull request."
nav_order = 50
nav_section = "Recettes"
+++
# Recette GitHub Actions

Un workflow simple qui lance le formateur, le linter et l'analyseur à chaque push et chaque pull request, avec des annotations natives sur les PR.

## Configuration rapide

Le workflow installe Mago via Composer, ajoutez-le donc d'abord à votre projet :

```sh
composer require --dev "heyjordanparker/mago-sharp:^0.3.1"
```

Puis créez `.github/workflows/mago.yml` :

```yaml
name: Mago Code Quality

on:
  push:
  pull_request:

jobs:
  mago:
    name: Run Mago Checks
    runs-on: ubuntu-latest
    env:
      GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
    steps:
      - name: Checkout
        uses: actions/checkout@v6

      - name: Set up PHP
        uses: shivammathur/setup-php@v2
        with:
          php-version: '8.4'

      - name: Install dependencies
        run: composer install --no-interaction --no-progress

      - name: Check formatting
        run: vendor/bin/mago format --check

      - name: Lint
        if: success() || failure()
        run: vendor/bin/mago lint

      - name: Analyze
        if: success() || failure()
        run: vendor/bin/mago analyze
```

Quelques notes sur la structure :

- Réglez `php-version` sur la version de PHP de votre projet. `composer install` fournit aussi à l'analyseur vos dépendances, dont il a besoin pour résoudre les symboles.
- Le premier appel à `vendor/bin/mago` télécharge le binaire correspondant depuis la release GitHub. `GITHUB_TOKEN` n'est pas exporté automatiquement vers les étapes, donc le job le passe explicitement pour éviter la limite de taux anonyme de GitHub sur les runners partagés.
- Séparer `format`, `lint` et `analyze` en étapes distinctes fait remonter les résultats des trois, même quand une étape antérieure échoue. Un seul `run:` combiné ferait court-circuit au premier échec et masquerait le reste.
- `if: success() || failure()` lance l'étape quand le job n'a pas été annulé, ce qui est ce que vous voulez ici. `always()` la lancerait aussi après des échecs de setup.
- Utilisez `mago format --check`, pas `--dry-run`. `--check` quitte avec un code non nul quand des fichiers ont besoin d'être formatés ; `--dry-run` n'affiche qu'un diff et quitte toujours zéro.
- Mago détecte GitHub Actions via la variable d'environnement `GITHUB_ACTIONS` et bascule automatiquement sur `--reporting-format=github`, produisant des annotations natives sur les PR. Aucune configuration supplémentaire requise.

+++
title = "Supprimer les problèmes"
description = "Comment un projet désactive une règle, pour tous ses fichiers ou pour ceux qu'il nomme, dans mago.toml."
nav_order = 30
nav_section = "Fondamentaux"
+++
# Supprimer les problèmes

Corriger le problème sous-jacent est presque toujours la bonne réponse. Parfois ce n'est pas le cas : code legacy, faux positifs, exceptions délibérées. Pour ces cas-là, un projet désactive la règle dans son `mago.toml`, pour tous ses fichiers ou pour ceux qu'il nomme.

Un commentaire est du texte dans chaque fichier que Mago lit. Un commentaire qui nomme un code de problème, comme `// @mago-ignore lint:no-shorthand-ternary` ou `/** @mago-expect analysis:mixed-operand */`, ne masque rien : chaque exception du projet se trouve dans un seul fichier, où une revue la voit.

## Règles du linter

Chaque règle sous `[linter.rules]` accepte `enabled` et `exclude`. `enabled = false` désactive la règle pour le projet, et `exclude` la désactive pour les chemins ou globs qu'il liste, tandis que toutes les autres règles s'y appliquent encore.

```toml
[linter.rules]
no-shorthand-ternary = { enabled = false }
no-eval = { exclude = ["src/Templating/Compiler.php"] }
cyclomatic-complexity = { threshold = 6, exclude = ["src/Legacy/", "**/*Test.php"] }
```

La [référence de configuration du linter](/tools/linter/configuration-reference/) liste toutes les options.

## Problèmes de l'analyseur

La liste `ignore` de l'analyseur masque les codes qu'elle nomme, partout, dans les chemins que nomme le `in` d'une entrée, ou là où le `pattern` d'une entrée correspond au texte du problème.

```toml
[analyzer]
ignore = [
    "mixed-argument",
    { code = "missing-return-type", in = "tests/" },
    { code = "unknown-class-instantiation", in = ["src/Hydrator.php"] },
    { pattern = "Symfony", code = "mixed-assignment" },
]
```

La [référence de configuration de l'analyseur](/tools/analyzer/configuration-reference/) décrit chaque forme. `mago analyze --skip-ignores` montre ce que la liste masque, le temps d'une exécution.

Une entrée `ignore` ne peut pas masquer une erreur du langage dans un fichier PHP# : l'erreur reste signalée, avec un avertissement `unsuppressible-error`. Elle masque une erreur d'une règle d'un hôte d'extension, dont le code nomme son hôte, comme `acme/no-raw-sql`, comme tout autre problème.

## Fichiers qu'un outil ignore

`excludes` sous `[linter]`, `[analyzer]` ou `[guard]` retire des fichiers de cet outil seul. `[source].excludes` les retire de tous les outils.

```toml
[guard]
excludes = ["src/ThirdParty/**"]
```

## Problèmes existants

Une [baseline](/fundamentals/baseline/) enregistre les problèmes qu'un projet a aujourd'hui, pour qu'une exécution ne signale que les nouveaux pendant que les anciens sont corrigés.

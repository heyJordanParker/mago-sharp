+++
title = "FAQ"
description = "Questions fréquentes sur Mago, le projet, et ce qui y a sa place ou non."
nav_order = 10
nav_section = "Référence"
+++
# FAQ

## Pourquoi le nom « Mago » ?

Le nom vient de Mago, le projet amont, qui s'appelait d'abord « fennec », d'après le fennec, renard du désert d'Afrique du Nord. Un conflit de nom avec un autre outil a forcé un changement.

Carthage Software, qui maintient le projet amont, l'a nommé d'après Mago de Carthage, un écrivain carthaginois antique connu comme le « Père de l'Agriculture ». Tout comme il cultivait la terre, l'outil vise à aider les développeurs à cultiver leurs bases de code.

Le nom a un double sens utile. En espagnol et en italien, « mago » signifie « magicien » ou « sorcier ». Le logo, dessiné pour le projet amont Mago, capture les deux : un fennec coiffé d'un chapeau et d'une robe de sorcier, avec l'ancien symbole carthaginois de Tanit sur ses vêtements.

## Comment prononce-t-on Mago ?

`/ˈmɑːɡoʊ/`, « ma-go ». Deux syllabes : « ma » comme dans « maman », « go » comme dans « go ».

## mago-sharp fournit-il un serveur de langage ?

Non. mago-sharp n'implémente pas le Language Server Protocol. Utilisez-le en ligne de commande ou en CI. La page [Configuration](/guide/configuration/) décrit le schéma JSON que les éditeurs utilisent pour valider `mago.toml`, ainsi que les liens de terminal qui ouvrent un fichier signalé dans votre éditeur.

## mago-sharp propose-t-il des extensions d'éditeur (VS Code, etc.) ?

Non. mago-sharp ne fournit aucune extension propre à un éditeur.

## mago-sharp prend-il en charge des plugins d'analyseur ?

Oui, par des extensions. Une extension est un programme externe que mago-sharp lance depuis `[extension-hosts]` dans `mago.toml` et avec lequel il communique par un protocole binaire de workers. Elle peut ajouter des règles au linter et des plugins à l'analyseur. Le formateur et le guard n'ont pas d'API d'extension. mago-sharp fournit un SDK PHP pour écrire des extensions : l'espace de noms `Mago\Sdk` du paquet Composer `heyjordanparker/mago-sharp`. La page [Extensions](/extensions/overview/) décrit le protocole, le SDK et un exemple complet.

## Quels outils mago-sharp inclut-il ?

Un seul binaire fournit :

- `mago lint`, le linter.
- `mago analyze`, l'analyseur statique, pour PHP et PHP#.
- `mago format`, le formateur.
- `mago guard`, qui fait respecter les règles d'architecture entre couches.
- `mago compile`, qui compile les fichiers PHP# pour le moteur PHP#.

`mago fix` applique les corrections du guard, de l'analyseur, du linter et du formateur jusqu'à ce qu'aucun d'eux ne change plus rien.

## Mago implémentera-t-il une alternative à Composer ?

Non. Composer est un outil fantastique, et l'essentiel de son travail est lié aux I/O. Une réécriture en Rust n'apporterait pas grand-chose en vitesse, fragmenterait l'écosystème et rendrait très difficile la prise en charge de l'architecture de plugins de Composer basée sur PHP.

## Mago implémentera-t-il un runtime PHP ?

Non. Le runtime PHP est énorme. Même de très gros efforts (HHVM de Facebook, KPHP de VK) ont eu du mal à atteindre la pleine parité avec le moteur Zend. Un projet plus petit ne peut pas faire mieux, et le résultat ne ferait que fragmenter la communauté. Mago se concentre sur l'outillage, pas sur les runtimes.

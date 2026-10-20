#!/bin/sh
# Pousser rscreen vers le Gitea interne SANS JAMAIS mémoriser le jeton dans le dépôt.
#
#   GITEA_TOKEN=xxxx sh scripts/push.sh
#
# Prérequis :
#   - gitlab.nirvanet.org doit résoudre (DNS interne ou /etc/hosts)
#   - la CA nirvanet dans le magasin système (ne pas désactiver la vérification TLS)
#   - le dépôt est privé : la poussée exige un jeton
set -e
: "${GITEA_TOKEN:?définir GITEA_TOKEN (jeton d'accès ; ne doit jamais être commité)}"
HOST=${GITEA_HOST:-gitlab.nirvanet.org}
REPO=${GITEA_REPO:-davy/rscreen}
BR=$(git symbolic-ref --short HEAD)
# L'URL authentifiée n'existe que le temps de la commande : origin reste neutre.
git -c credential.helper= push "https://oauth2:${GITEA_TOKEN}@${HOST}/${REPO}.git" "$BR:$BR"
echo "poussé : $(git rev-parse --short HEAD) -> ${HOST}/${REPO}#$BR"

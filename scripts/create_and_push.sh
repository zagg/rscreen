#!/bin/sh
# Crée le dépôt rscreen sur le Gitea interne s'il manque, puis pousse main.
# Le jeton ne transite QUE dans l'URL de la commande push : ni commit, ni
# remote permanent, ni fichier de credentials.
#
#   GITEA_TOKEN=xxxx sh scripts/create_and_push.sh
set -e
: "${GITEA_TOKEN:?définir GITEA_TOKEN (jeton d'accès ; jamais commité)}"
HOST=${GITEA_HOST:-gitlab.nirvanet.org}
REPO=${GITEA_REPO:-davy/rscreen}
OWNER=${REPO%%/*}
NAME=${REPO##*/}
API="https://${HOST}/api/v1"
AUTH="Authorization: token ${GITEA_TOKEN}"
BR=$(git symbolic-ref --short HEAD)

code=$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" "$API/repos/${REPO}")
if [ "$code" = "404" ] || [ "$code" = "401" ]; then
    echo "création du dépôt ${REPO}…"
    curl -s -o /dev/null -w 'http=%{http_code}\n' -X POST \
        -H "$AUTH" -H 'Content-Type: application/json' \
        -d "{\"name\":\"${NAME}\",\"private\":true,\"auto_init\":false,\"default_branch\":\"${BR}\"}" \
        "$API/user/repos"
fi

if ! git remote get-url origin >/dev/null 2>&1; then
    git remote add origin "https://${HOST}/${REPO}.git"
fi

git -c credential.helper= push "https://oauth2:${GITEA_TOKEN}@${HOST}/${REPO}.git" "${BR}:${BR}"
echo "poussé : $(git rev-parse --short HEAD) -> ${HOST}/${REPO}#${BR}"
echo "branches distantes :"
curl -s -H "$AUTH" "$API/repos/${REPO}/branches" | tr ',' '\n' | grep '"name"' || true

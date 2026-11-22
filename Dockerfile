# syntax=docker/dockerfile:1.7
#
# Images minimales : un binaire statique (musl) dans une image `scratch` (pas de shell, pas de
# distribution, pas de gestionnaire de paquets), exécuté par un utilisateur non-root.
#
#   docker build --target master -t nxt-master .
#   docker build --target agent  -t nxt-agent  .
#
# Binaires statiques seuls (sans image), pour les publier ou les copier sur une machine :
#   docker buildx build --target artifacts --output type=local,dest=dist/linux .
#   (produit nxt-master et nxt-agent pour l'architecture de la machine de build : amd64 ou arm64)

# ---------- Interface web (Svelte) ----------
# Construite ici puis EMBARQUÉE dans le binaire du master : l'image finale ne contient aucun Node.
FROM node:26-alpine AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

# ---------- Compilation ----------
FROM rust:alpine AS build
RUN apk add --no-cache build-base
WORKDIR /src
COPY . .
COPY --from=ui /ui/dist /src/ui/dist
# Peu de parallélisme : la compilation de rustls/sqlite saturerait la mémoire d'un Docker Desktop modeste.
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p master -p agent \
 && mkdir /out \
 && cp target/release/master /out/nxt-master \
 && cp target/release/agent /out/nxt-agent
# Dossiers vides appartenant à l'utilisateur non-root (65532 = « nonroot » des images distroless).
RUN mkdir /data /tmp-dir \
 && chown 65532:65532 /data /tmp-dir \
 && chmod 0700 /data \
 && chmod 1777 /tmp-dir

# ---------- Binaires seuls ----------
# Sortie de fichiers (`--output type=local`) : les deux binaires statiques, rien d'autre.
FROM scratch AS artifacts
COPY --from=build /out/nxt-master /nxt-master
COPY --from=build /out/nxt-agent /nxt-agent

# ---------- Master ----------
FROM scratch AS master
COPY --from=build /out/nxt-master /nxt-master
COPY --from=build --chown=65532:65532 --chmod=0700 /data /data
COPY --from=build --chmod=1777 /tmp-dir /tmp
# API d'administration : dans un conteneur, « 127.0.0.1 » est inaccessible depuis l'hôte ; on écoute donc
# sur toutes les interfaces DU CONTENEUR et c'est la publication du port qui décide de l'exposition
# (compose.example.yml la limite à 127.0.0.1 de l'hôte). Ne jamais la publier sans proxy TLS.
ENV MASTER_DATA_DIR=/data \
    MASTER_LISTEN=0.0.0.0:7443 \
    MASTER_ADMIN_LISTEN=0.0.0.0:7080 \
    MASTER_ADMIN_ALLOW_REMOTE=1
USER 65532:65532
EXPOSE 7443 7080
VOLUME ["/data"]
# MASTER_SANS (noms/IP sous lesquels les agents joignent ce master) est OBLIGATOIRE.
HEALTHCHECK --interval=15s --timeout=3s --start-period=10s --retries=3 \
    CMD ["/nxt-master", "healthcheck"]
ENTRYPOINT ["/nxt-master"]
CMD ["serve"]

# ---------- Agent ----------
# Aucun port exposé : l'agent initie toujours la connexion vers le master.
# L'API du client torrent doit rester sur la boucle locale : partager la pile réseau du conteneur
# du client (`network_mode: "service:<client>"`), cf. deploy/docker/compose.example.yml.
FROM scratch AS agent
COPY --from=build /out/nxt-agent /nxt-agent
COPY --from=build --chown=65532:65532 --chmod=0700 /data /data
COPY --from=build --chmod=1777 /tmp-dir /tmp
ENV AGENT_DATA_DIR=/data
USER 65532:65532
VOLUME ["/data"]
ENTRYPOINT ["/nxt-agent"]
CMD ["run"]

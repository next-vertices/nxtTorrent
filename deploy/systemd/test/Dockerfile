# Machine de test avec systemd comme PID 1 (pour exercer RÉELLEMENT les unités).
FROM ubuntu:24.04
RUN apt-get update \
 && apt-get install -y --no-install-recommends systemd systemd-sysv dbus sudo iproute2 procps ca-certificates \
 && apt-get clean && rm -rf /var/lib/apt/lists/*
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]

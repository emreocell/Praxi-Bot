# A fresh Ubuntu 24.04 that boots systemd as PID 1, for scripts/linux/tests/e2e/run.sh.
# Only what a real VPS image has (systemd, dbus, sudo) plus the test's own tools (jq, iproute2).
# Everything Praxi needs (curl, libssl, libdbus, Node.js, ...) is installed by the installer itself.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive container=docker
RUN apt-get update \
 && apt-get install -y --no-install-recommends systemd systemd-sysv dbus sudo jq iproute2 \
 && rm -rf /var/lib/apt/lists/* \
 # Units that only make sense on real hardware and slow the boot down.
 && systemctl mask systemd-udevd.service systemd-udev-trigger.service systemd-resolved.service \
      systemd-timesyncd.service getty.target console-getty.service \
 # An ordinary sudo-capable login user, like the "ubuntu" user of a cloud image: `sudo praxi ...`.
 && echo 'ubuntu ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-e2e \
 && chmod 0440 /etc/sudoers.d/90-e2e

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]

# Multi-stage Dockerfile for the Praxi always-on host (`bot serve --host`).
#
# Base (headless daemon only):
#   docker build -f packaging/docker/Dockerfile -t praxi-host .
#
# Desktop + browser variant (Xvfb 1920x1080x24 + minimal Xfce + Chrome for Testing):
#   docker build --build-arg WITH_DESKTOP=1 -f packaging/docker/Dockerfile -t praxi-host:desktop .

# Pinned Rust toolchain on Debian 12 (Bookworm); bump alongside the runtime tag.
FROM rust:1.99.1-bookworm AS builder

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

WORKDIR /src

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libdbus-1-dev \
        libssl-dev \
        libxcb1-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .

# Use the image's preinstalled toolchain directly so rust-toolchain.toml
# (channel = "stable") does not trigger an extra rustup download at build time.
RUN toolchain="$(rustup toolchain list | awk 'NR==1 {print $1}')" \
    && RUSTUP_TOOLCHAIN="${toolchain}" cargo build --release --locked -p cli \
    && (strip /src/target/release/bot || true)

# Pinned Debian Bookworm slim date tag (multi-arch: amd64, arm64).
# Update to a newer bookworm-YYYYMMDD-slim tag from Docker Hub as needed.
FROM debian:bookworm-20261104-slim AS runtime

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG WITH_DESKTOP=0
ARG TARGETARCH
ARG PRAXI_UID=10001
ARG PRAXI_GID=10001

# Same pinned Chrome for Testing version and checksums as scripts/install-linux.sh.
ARG CFT_VERSION=157.0.8103.38
ARG CFT_SHA256_LINUX64=8cdeb344e9b2cefe59f4aea8d1d93d5d93054c03cde217665c16fc1f62e72a66
ARG CFT_SHA256_LINUX_ARM64=5517510e61657ff43ee9ab804ecd1a499b3d5bd3caf72f097516bd6565ba5a17

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        libdbus-1-3 \
        libssl3 \
        tini \
    && if [ "${WITH_DESKTOP}" = "1" ]; then \
        apt-get install -y --no-install-recommends \
            xvfb \
            xauth \
            at-spi2-core \
            x11-utils \
            x11-xserver-utils \
            xdg-utils \
            dbus \
            dbus-x11 \
            xfwm4 \
            xfce4-panel \
            xfdesktop4 \
            xfconf \
            xfce4-settings \
            desktop-file-utils \
            libatk-adaptor \
            libgail-common \
            libglib2.0-bin \
            tango-icon-theme \
            fonts-liberation \
            fonts-noto-core \
            fonts-noto-color-emoji \
            fonts-noto-cjk \
            unzip \
            libnss3 \
            libatk1.0-0 \
            libatk-bridge2.0-0 \
            libcups2 \
            libdrm2 \
            libxkbcommon0 \
            libxcomposite1 \
            libxdamage1 \
            libxfixes3 \
            libxrandr2 \
            libgbm1 \
            libpango-1.0-0 \
            libcairo2 \
            libasound2 \
            libatspi2.0-0; \
    fi \
    && rm -rf /var/lib/apt/lists/*

# Install official Google Chrome for Testing when WITH_DESKTOP=1.
# Note: in a container, Chrome's user-namespace sandbox requires --security-opt
# seccomp/apparmor overrides on hosts that restrict unprivileged userns; see
# docs/guide/docker-host.md. AppArmor profiles cannot be loaded during docker build.
RUN if [ "${WITH_DESKTOP}" = "1" ]; then \
        arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
        case "${arch}" in \
            amd64|x86_64) platform="linux64"; expected_sha="${CFT_SHA256_LINUX64}" ;; \
            arm64|aarch64) platform="linux-arm64"; expected_sha="${CFT_SHA256_LINUX_ARM64}" ;; \
            *) echo "unsupported architecture '${arch}'" >&2; exit 1 ;; \
        esac; \
        url="https://storage.googleapis.com/chrome-for-testing-public/${CFT_VERSION}/${platform}/chrome-${platform}.zip"; \
        work="$(mktemp -d)"; \
        curl -fsSL --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 300 \
            --retry 3 --retry-delay 5 --retry-connrefused "${url}" -o "${work}/chrome.zip"; \
        echo "${expected_sha}  ${work}/chrome.zip" | sha256sum -c -; \
        unzip -q "${work}/chrome.zip" -d "${work}"; \
        install -d -m 0755 /opt/praxi-browser; \
        cp -a "${work}/chrome-${platform}/." /opt/praxi-browser/; \
        chown -R root:root /opt/praxi-browser; \
        rm -rf "${work}"; \
        missing="$(ldd /opt/praxi-browser/chrome 2>/dev/null | awk '/not found/ {print $1}' | sort -u | tr '\n' ' ')"; \
        if [ -n "${missing}" ]; then \
            echo "missing Chrome shared libraries: ${missing}" >&2; \
            exit 1; \
        fi; \
        /opt/praxi-browser/chrome --version; \
        install -d -o root -g root -m 1777 /tmp/.X11-unix; \
    fi

RUN groupadd --system --gid "${PRAXI_GID}" praxi \
    && useradd --system --uid "${PRAXI_UID}" --gid praxi \
        --home-dir /var/lib/praxi --no-create-home \
        --shell /usr/sbin/nologin praxi \
    && install -d -m 0750 -o praxi -g praxi /var/lib/praxi \
    && install -d -m 0755 -o root -g root /opt/praxi/bin

COPY --from=builder /src/target/release/bot /opt/praxi/bin/bot
COPY --from=builder /src/LICENSE /src/NOTICE /src/THIRD_PARTY_NOTICES.md /opt/praxi/
COPY packaging/docker/entrypoint.sh /opt/praxi/bin/entrypoint.sh
COPY packaging/docker/bot-wrapper.sh /usr/local/bin/bot

RUN sed -i 's/\r$//' /opt/praxi/bin/entrypoint.sh /usr/local/bin/bot \
    && chmod 0755 /opt/praxi/bin/bot /opt/praxi/bin/entrypoint.sh /usr/local/bin/bot \
    && ln -sf /usr/local/bin/bot /usr/local/bin/praxi

ENV BOT_DATA_DIR=/var/lib/praxi \
    HOME=/var/lib/praxi \
    BOT_PORT=47821 \
    BOT_SECRETS_BACKEND=file \
    PATH="/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin"

VOLUME ["/var/lib/praxi"]
WORKDIR /var/lib/praxi

# `bot serve` binds 127.0.0.1:47821 only; never expose this port publicly.
EXPOSE 47821

USER praxi

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["sh", "-c", "curl -fsS --noproxy '*' \"http://127.0.0.1:${BOT_PORT:-47821}/v1/health\" | grep -Eq '\"status\"[[:space:]]*:[[:space:]]*\"(ok|setup_required)\"'"]

ENTRYPOINT ["/usr/bin/tini", "--", "/opt/praxi/bin/entrypoint.sh"]
CMD ["serve", "--host"]

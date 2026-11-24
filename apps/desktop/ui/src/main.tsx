import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "ui/tokens.css";
import "@fontsource-variable/geist/wght.css";
import "@fontsource-variable/geist-mono/wght.css";
import "@fontsource/opendyslexic/400.css";
import "@fontsource/opendyslexic/700.css";
import { ApiClient } from "./api/client";
import { App } from "./App";
import "./global.css";
import "./i18n";
import { isTauriRuntime } from "./overlay/events";
import { OverlayWindow } from "./overlay/OverlayWindow";
import { WidgetWindow } from "./overlay/WidgetWindow";
import { paintSavedAppearanceEarly } from "./state/useApplyAppearance";

// The owner key of this app's daemon (main window only; the overlay and widget windows get none).
// Requested once and kept in memory; every call to THIS computer's daemon carries it.
let ownerKey: Promise<string | null> | null = null;
if (isTauriRuntime()) {
  ApiClient.ownerKeyProvider = () => (ownerKey ??= invoke<string>("owner_key").catch(() => null));
}

type WindowKind = "main" | "overlay" | "widget";

function resolveWindowKind(): WindowKind {
  if (typeof window !== "undefined") {
    const queryKind = new URLSearchParams(window.location.search).get("window");
    if (queryKind === "overlay" || queryKind === "widget") {
      return queryKind;
    }
    if (window.location.hash === "#overlay") {
      return "overlay";
    }
    if (window.location.hash === "#widget") {
      return "widget";
    }
    if (isTauriRuntime()) {
      try {
        const label = getCurrentWebviewWindow().label;
        if (label.startsWith("overlay")) {
          return "overlay";
        }
        if (label === "widget") {
          return "widget";
        }
      } catch {
        // Fall back to main window
      }
    }
  }
  return "main";
}

const windowKind = resolveWindowKind();
document.documentElement.dataset.windowKind = windowKind;

paintSavedAppearanceEarly(windowKind !== "main");

// Layout probe: same guard as state/useQuotas.ts, so production builds drop it.
if (
  (import.meta.env.DEV || import.meta.env.VITE_MOCK === "1") &&
  ["layout", "search", "content"].includes(new URLSearchParams(window.location.search).get("probe") ?? "")
) {
  const stored = new URLSearchParams(window.location.search).get("stored");
  if (stored) {
    // A layout saved by an older session, read by App's first render.
    try {
      window.localStorage.setItem("praxi.shell.layout.v1", JSON.stringify({ rail: 264, panel: Number(stored) }));
    } catch {
      // Storage unavailable: the probe then runs with the defaults.
    }
  }
  void import("./dev/layoutProbe")
    .then((m) => {
        const mode = new URLSearchParams(window.location.search).get("probe");
        return mode === "search" ? m.runSearchProbe() : mode === "content" ? m.runContentProbe() : m.runLayoutProbe();
      })
    .catch((e: unknown) => {
      document.documentElement.dataset.probeError = String(e);
    });
}

const container = document.getElementById("root");
if (!container) {
  throw new Error("Root container #root not found");
}

const root = createRoot(container);

// Dev/mock-only avatar review gallery (`?mock=1&gallery=avatars`); dropped from production builds.
const showAvatarGallery =
  (import.meta.env.DEV || import.meta.env.VITE_MOCK === "1") &&
  new URLSearchParams(window.location.search).get("gallery") === "avatars";

if (showAvatarGallery) {
  void import("./dev/AvatarGallery").then(({ AvatarGallery }) => {
    root.render(
      <StrictMode>
        <AvatarGallery />
      </StrictMode>
    );
  });
} else {
  root.render(
    <StrictMode>
      {windowKind === "overlay" ? (
        <OverlayWindow />
      ) : windowKind === "widget" ? (
        <WidgetWindow />
      ) : (
        <App />
      )}
    </StrictMode>
  );
}


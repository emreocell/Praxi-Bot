import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "ui/tokens.css";
import "@fontsource-variable/geist/wght.css";
import "@fontsource-variable/geist-mono/wght.css";
import { App } from "./App";
import "./global.css";
import "./i18n";
import { isTauriRuntime } from "./overlay/events";
import { OverlayWindow } from "./overlay/OverlayWindow";
import { WidgetWindow } from "./overlay/WidgetWindow";
import { paintSavedAppearanceEarly } from "./state/useApplyAppearance";

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

const container = document.getElementById("root");
if (!container) {
  throw new Error("Root container #root not found");
}

createRoot(container).render(
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
